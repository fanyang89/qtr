use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use uuid::Uuid;
use virt::{connect::Connect, domain::Domain, error::ErrorNumber, network::Network, sys};

use crate::{
    config::{
        DiskFormat, FedoraMirror, GraphicsMode, VmInstallArgs, VmInstallCommand,
        VmInstallFedoraArgs,
    },
    disk, guest_agent,
    vm::{
        self, VmCpu, VmCpuMode, VmDisk, VmDiskBus, VmDiskEntry, VmDiskIoTuneConfig, VmDiskSerial,
        VmDiskType, VmInterface, VmInterfaceEntry, VmInterfaceType, VmManifest, VmMemory,
        VmOptionalValue,
    },
};

pub fn run(args: VmInstallArgs) -> Result<()> {
    match args.command {
        VmInstallCommand::Fedora(args) => {
            install_fedora_with_control(args, &InstallControl::default())
        }
    }
}

type PhaseReporter = dyn Fn(&'static str) + Send + Sync;

#[derive(Clone)]
pub struct InstallControl {
    cancelled: Arc<AtomicBool>,
    report_phase: Arc<PhaseReporter>,
}

impl Default for InstallControl {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            report_phase: Arc::new(|_| {}),
        }
    }
}

impl InstallControl {
    pub fn new(
        cancelled: Arc<AtomicBool>,
        report_phase: impl Fn(&'static str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            cancelled,
            report_phase: Arc::new(report_phase),
        }
    }

    fn phase(&self, phase: &'static str) -> Result<()> {
        (self.report_phase)(phase);
        self.check()
    }

    fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            bail!("installation cancelled");
        }
        Ok(())
    }
}

struct InstallPlan {
    args: VmInstallFedoraArgs,
    iso: PathBuf,
    disk: PathBuf,
    output: PathBuf,
    ssh_key_material: String,
    hostname: String,
    serial_log: PathBuf,
    install_log: PathBuf,
    manifest: VmManifest,
    manifest_yaml: String,
    kickstart: String,
    disk_size_bytes: u64,
}

#[derive(Default)]
struct OwnedResources {
    domain_uuid: Option<String>,
    disk_identity: Option<FileIdentity>,
    work_dir: Option<PathBuf>,
    committed: bool,
}

#[derive(Clone, Copy)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

pub fn install_fedora_with_control(
    args: VmInstallFedoraArgs,
    control: &InstallControl,
) -> Result<()> {
    control.phase("planning")?;
    let plan = build_plan(args)?;
    control.phase("preflight")?;
    preflight(&plan)?;
    control.check()?;
    if plan.args.dry_run {
        print_plan(&plan);
        return Ok(());
    }

    let mut owned = OwnedResources::default();
    let result = execute_install(&plan, &mut owned, control);
    if let Err(error) = result {
        if !owned.committed && !plan.args.keep_failed {
            if let Err(cleanup_error) = rollback(&plan, &mut owned) {
                eprintln!("[qtr] warning: installation cleanup failed: {cleanup_error:#}");
            }
        } else if owned.committed && !plan.args.keep_failed {
            cleanup_work_dir(&mut owned);
            eprintln!("[qtr] installed VM was preserved after post-install failure");
            if plan.output.exists() {
                eprintln!("[qtr] VM manifest: {}", plan.output.display());
            } else {
                eprintln!(
                    "[qtr] recover the manifest with: qtr vm dump {} -o {}",
                    plan.args.name,
                    plan.output.display()
                );
            }
        } else if let Some(work_dir) = &owned.work_dir {
            eprintln!(
                "[qtr] preserved failed installation work directory: {}",
                work_dir.display()
            );
        }
        return Err(error);
    }
    Ok(())
}

fn build_plan(mut args: VmInstallFedoraArgs) -> Result<InstallPlan> {
    if args.name.trim().is_empty() {
        bail!("VM name must not be empty");
    }
    if args.memory_mib == 0 || args.vcpus == 0 {
        bail!("memory-mib and vcpus must be greater than zero");
    }
    if args.timeout_secs == 0 || args.verify_timeout_secs == 0 {
        bail!("installation and verification timeouts must be greater than zero");
    }
    let cwd = std::env::current_dir().context("failed to determine current directory")?;
    let iso = canonical_file(&args.iso, "Fedora ISO")?;
    let ssh_key_path = canonical_file(&args.ssh_key, "SSH public key")?;
    let disk = canonical_destination(&cwd, &args.disk, "target disk")?;
    let output = canonical_destination(&cwd, &args.output, "output manifest")?;
    args.iso = iso.clone();
    args.ssh_key = ssh_key_path.clone();
    args.disk = disk.clone();
    args.output = output.clone();
    let ssh_key = fs::read_to_string(&ssh_key_path)
        .with_context(|| format!("failed to read SSH public key {}", ssh_key_path.display()))?;
    let ssh_key = validate_ssh_key(&ssh_key)?.to_string();
    let ssh_key_material = ssh_key
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let hostname = args.hostname.clone().unwrap_or_else(|| args.name.clone());
    validate_hostname(&hostname)?;
    let disk_size_bytes = vm::parse_disk_size_bytes(&args.disk_size)?;
    let serial_log = args
        .serial_log
        .clone()
        .map(|path| canonical_destination(&cwd, &path, "serial log"))
        .transpose()?
        .unwrap_or_else(|| output.with_extension("serial.log"));
    let install_log = args
        .install_log
        .clone()
        .map(|path| canonical_destination(&cwd, &path, "install log"))
        .transpose()?
        .unwrap_or_else(|| output.with_extension("install.log"));
    args.serial_log = Some(serial_log.clone());
    args.install_log = Some(install_log.clone());
    ensure_distinct_paths(&[&disk, &output, &serial_log, &install_log])?;
    let manifest = fedora_manifest(&args, &disk, &serial_log);
    let manifest_yaml = vm::serialize_manifest_yaml(&manifest)?;
    let kickstart = render_fedora_kickstart(&hostname, &ssh_key_material, args.mirror);

    Ok(InstallPlan {
        args,
        iso,
        disk,
        output,
        ssh_key_material,
        hostname,
        serial_log,
        install_log,
        manifest,
        manifest_yaml,
        kickstart,
        disk_size_bytes,
    })
}

fn preflight(plan: &InstallPlan) -> Result<()> {
    if plan.args.connect_uri != "qemu:///system" {
        bail!("Fedora installation currently supports only qemu:///system");
    }
    for tool in [
        "blkid",
        "qemu-img",
        "virt-install",
        "osinfo-detect",
        "ksvalidator",
        "ssh-keygen",
    ] {
        which::which(tool).with_context(|| format!("required command {tool:?} was not found"))?;
    }
    for (path, label) in [
        (&plan.disk, "target disk"),
        (&plan.output, "output manifest"),
        (&plan.serial_log, "serial log"),
        (&plan.install_log, "install log"),
    ] {
        if path.exists() {
            bail!("{label} already exists: {}", path.display());
        }
    }
    detect_fedora_server_iso(&plan.iso)?;
    validate_ssh_key_file(&plan.args.ssh_key)?;

    let conn = Connect::open(Some(&plan.args.connect_uri))
        .with_context(|| format!("failed to connect to libvirt at {}", plan.args.connect_uri))?;
    if conn
        .list_all_domains(0)?
        .into_iter()
        .any(|domain| domain.get_name().as_deref() == Ok(plan.args.name.as_str()))
    {
        bail!("domain {} already exists", plan.args.name);
    }
    let network = Network::lookup_by_name(&conn, &plan.args.network)
        .with_context(|| format!("libvirt network {} was not found", plan.args.network))?;
    if !network.is_active()? {
        bail!("libvirt network {} is inactive", plan.args.network);
    }
    Ok(())
}

fn execute_install(
    plan: &InstallPlan,
    owned: &mut OwnedResources,
    control: &InstallControl,
) -> Result<()> {
    control.phase("workspace")?;
    let work_dir = plan
        .output
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".qtr-install-{}", Uuid::new_v4()));
    fs::create_dir(&work_dir)
        .with_context(|| format!("failed to create work directory {}", work_dir.display()))?;
    fs::set_permissions(&work_dir, fs::Permissions::from_mode(0o700))?;
    owned.work_dir = Some(work_dir.clone());
    let kickstart_path = work_dir.join("ks.cfg");
    write_private_file(&kickstart_path, plan.kickstart.as_bytes())?;
    validate_kickstart(&kickstart_path)?;

    control.phase("disk")?;
    owned.disk_identity = Some(create_disk_atomic(&plan.disk, plan.disk_size_bytes)?);
    control.phase("domain")?;
    let domain_uuid = Uuid::new_v4().to_string();
    owned.domain_uuid = Some(domain_uuid.clone());
    vm::define_new_by_manifest(&plan.args.connect_uri, plan.manifest.clone(), &domain_uuid)
        .map_err(anyhow::Error::new)?;

    eprintln!(
        "[qtr] installing Fedora; log: {}",
        plan.install_log.display()
    );
    control.phase("installing")?;
    run_virt_install(plan, &kickstart_path, control)?;
    control.phase("committing")?;
    ensure_domain_inactive(&plan.args.connect_uri, &plan.args.name)?;
    vm::remove_cdrom_by_media_path(&plan.args.connect_uri, &plan.args.name, &plan.iso)?;
    vm::apply_by_manifest(&plan.args.connect_uri, plan.manifest.clone())
        .map_err(anyhow::Error::new)?;
    owned.committed = true;
    write_atomic(&plan.output, plan.manifest_yaml.as_bytes())?;

    control.phase("starting")?;
    vm::start_by_name(&plan.args.connect_uri, &plan.args.name).map_err(anyhow::Error::new)?;
    control.phase("verifying")?;
    verify_installed_guest(plan)?;
    control.phase("cleanup")?;
    if work_dir.exists() {
        fs::remove_dir_all(&work_dir)
            .with_context(|| format!("failed to remove work directory {}", work_dir.display()))?;
    }
    owned.work_dir = None;
    eprintln!("[qtr] Fedora VM installed and verified: {}", plan.args.name);
    eprintln!("[qtr] VM manifest: {}", plan.output.display());
    Ok(())
}

fn run_virt_install(
    plan: &InstallPlan,
    kickstart_path: &Path,
    control: &InstallControl,
) -> Result<()> {
    let log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&plan.install_log)
        .with_context(|| {
            format!(
                "failed to create install log {}",
                plan.install_log.display()
            )
        })?;
    let stderr = log.try_clone()?;
    let args = virt_install_args(plan, kickstart_path);
    let mut child = Command::new("virt-install")
        .args(&args)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("failed to start virt-install")?;
    let deadline = Instant::now() + Duration::from_secs(plan.args.timeout_secs);
    loop {
        if let Err(error) = control.check() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        if let Some(status) = child
            .try_wait()
            .context("failed to wait for virt-install")?
        {
            if status.success() {
                return Ok(());
            }
            bail!(
                "virt-install failed with status {status}; see {}",
                plan.install_log.display()
            );
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "Fedora installation timed out after {} seconds; see {}",
                plan.args.timeout_secs,
                plan.install_log.display()
            );
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn virt_install_args(plan: &InstallPlan, kickstart_path: &Path) -> Vec<String> {
    vec![
        "--connect".to_string(),
        plan.args.connect_uri.clone(),
        "--reinstall".to_string(),
        plan.args.name.clone(),
        "--location".to_string(),
        plan.iso.display().to_string(),
        "--osinfo".to_string(),
        "detect=on,require=on".to_string(),
        "--initrd-inject".to_string(),
        kickstart_path.display().to_string(),
        "--extra-args".to_string(),
        "inst.ks=file:/ks.cfg console=ttyS0,115200n8".to_string(),
        "--noautoconsole".to_string(),
        "--wait=-1".to_string(),
        "--noreboot".to_string(),
    ]
}

fn verify_installed_guest(plan: &InstallPlan) -> Result<()> {
    let conn = Connect::open(Some(&plan.args.connect_uri))?;
    let domain = Domain::lookup_by_name(&conn, &plan.args.name)?;
    let deadline =
        guest_agent::GuestAgentDeadline::new(Duration::from_secs(plan.args.verify_timeout_secs));
    guest_agent::wait_ready_with_deadline(&domain, &deadline)
        .context("QEMU Guest Agent did not become ready after installation")?;
    let mirror_check = match plan.args.mirror {
        FedoraMirror::Official => "true",
        FedoraMirror::Tuna => {
            "grep -q 'mirrors.tuna.tsinghua.edu.cn/fedora' /etc/yum.repos.d/fedora.repo"
        }
    };
    let command = format!(
        "set -eu; \
         test \"$(. /etc/os-release; printf %s \"$ID\")\" = fedora; \
         test \"$(getenforce)\" = Disabled; \
         grep -qw selinux=0 /proc/cmdline; \
         test \"$(systemctl is-enabled firewalld.service 2>/dev/null || true)\" = masked; \
         test \"$(systemctl is-active firewalld.service 2>/dev/null || true)\" = inactive; \
         systemctl is-enabled --quiet qemu-guest-agent.service; \
         systemctl is-active --quiet qemu-guest-agent.service; \
         id qtr >/dev/null; \
         grep -Fq '{}' /home/qtr/.ssh/authorized_keys; \
         test -f /var/lib/qtr/install-complete; \
        {mirror_check}",
        plan.ssh_key_material
    );
    let result = guest_agent::run_command_with_deadline(&domain, &command, &deadline)
        .context("failed to verify installed Fedora guest")?;
    if result.exitcode != 0 {
        bail!(
            "installed Fedora guest verification failed with exit code {}: {}",
            result.exitcode,
            String::from_utf8_lossy(&result.stderr)
        );
    }
    Ok(())
}

fn rollback(plan: &InstallPlan, owned: &mut OwnedResources) -> Result<()> {
    let mut errors = Vec::new();
    let mut domain_removed = owned.domain_uuid.is_none();
    if let Some(uuid) = owned.domain_uuid.as_deref() {
        match remove_owned_domain(&plan.args.connect_uri, &plan.args.name, uuid) {
            Ok(()) => {
                domain_removed = true;
                owned.domain_uuid = None;
            }
            Err(error) => errors.push(format!("failed to remove domain safely: {error:#}")),
        }
    }
    if domain_removed
        && let Some(identity) = owned.disk_identity
        && plan.disk.exists()
    {
        match file_identity(&plan.disk) {
            Ok(current) if current.device == identity.device && current.inode == identity.inode => {
                if let Err(error) = fs::remove_file(&plan.disk) {
                    errors.push(format!(
                        "failed to remove disk {}: {error}",
                        plan.disk.display()
                    ));
                } else {
                    owned.disk_identity = None;
                }
            }
            Ok(_) => errors.push(format!(
                "refusing to remove replaced disk {}",
                plan.disk.display()
            )),
            Err(error) => errors.push(format!(
                "failed to verify disk {}: {error:#}",
                plan.disk.display()
            )),
        }
    }
    if domain_removed
        && plan.serial_log.exists()
        && let Err(error) = fs::remove_file(&plan.serial_log)
    {
        errors.push(format!(
            "failed to remove serial log {}: {error}",
            plan.serial_log.display()
        ));
    }
    if let Some(work_dir) = owned.work_dir.take()
        && work_dir.exists()
        && let Err(error) = fs::remove_dir_all(&work_dir)
    {
        errors.push(format!("failed to remove {}: {error}", work_dir.display()));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!(errors.join("; "))
    }
}

fn remove_owned_domain(connect_uri: &str, name: &str, uuid: &str) -> Result<()> {
    let conn = Connect::open(Some(connect_uri))?;
    let domain = match Domain::lookup_by_uuid_string(&conn, uuid) {
        Ok(domain) => domain,
        Err(error) if error.code() == ErrorNumber::NoDomain => {
            match Domain::lookup_by_name(&conn, name) {
                Ok(_) => bail!("domain name {name} now belongs to a different UUID"),
                Err(error) if error.code() == ErrorNumber::NoDomain => return Ok(()),
                Err(error) => {
                    return Err(anyhow::Error::new(error)
                        .context("failed to verify domain name during cleanup"));
                }
            }
        }
        Err(error) => {
            return Err(anyhow::Error::new(error).context("failed to look up domain by UUID"));
        }
    };
    if domain.is_active().context("failed to query domain state")? {
        domain.destroy().context("failed to stop active domain")?;
    }
    if domain
        .is_active()
        .context("failed to verify domain stopped")?
    {
        bail!("domain remained active after destroy");
    }
    domain
        .undefine_flags(sys::VIR_DOMAIN_UNDEFINE_MANAGED_SAVE)
        .context("failed to undefine domain")?;
    Ok(())
}

fn cleanup_work_dir(owned: &mut OwnedResources) {
    if let Some(work_dir) = owned.work_dir.take()
        && work_dir.exists()
        && let Err(error) = fs::remove_dir_all(&work_dir)
    {
        eprintln!(
            "[qtr] warning: failed to remove work directory {}: {error}",
            work_dir.display()
        );
    }
}

fn fedora_manifest(args: &VmInstallFedoraArgs, disk: &Path, serial_log: &Path) -> VmManifest {
    VmManifest {
        name: args.name.clone(),
        machine: None,
        cpu: Some(VmCpu {
            mode: VmCpuMode::HostPassthrough,
            model: None,
            vcpus: Some(args.vcpus),
            topology: None,
        }),
        memory: Some(VmMemory {
            size_mib: args.memory_mib,
            max_mib: None,
        }),
        io_threads: None,
        disks: vec![VmDiskEntry::present(VmDisk {
            id: Some("root".to_string()),
            disk_type: VmDiskType::File,
            path: disk.to_path_buf(),
            format: DiskFormat::Qcow2,
            target: Some("vda".to_string()),
            bus: VmDiskBus::VirtioBlk,
            cache: None,
            io: None,
            discard: None,
            detect_zeroes: None,
            readonly: None,
            serial: VmDiskSerial::default(),
            io_tune: VmDiskIoTuneConfig::default(),
        })],
        cdrom: None,
        cdroms: Some(Vec::new()),
        boot: Some(vec!["hd".to_string()]),
        memory_gib: 4,
        vcpus: 2,
        network: None,
        interfaces: Some(vec![VmInterfaceEntry::present(VmInterface {
            id: "primary".to_string(),
            interface_type: VmInterfaceType::Network,
            source: Some(args.network.clone()),
            model: "virtio".to_string(),
            mac: None,
            mode: VmOptionalValue::default(),
            vlan: VmOptionalValue::default(),
            mtu: VmOptionalValue::default(),
            link: VmOptionalValue::default(),
        })]),
        graphics: GraphicsMode::Vnc,
        vnc_listen: "127.0.0.1".to_string(),
        vnc_port: None,
        serial_log: Some(serial_log.to_path_buf()),
    }
}

fn render_fedora_kickstart(hostname: &str, ssh_key: &str, mirror: FedoraMirror) -> String {
    let mirror_post = match mirror {
        FedoraMirror::Official => String::new(),
        FedoraMirror::Tuna => r#"
sed -e 's|^metalink=|#metalink=|g' \
    -e 's|^#baseurl=http://download.example/pub/fedora/linux|baseurl=https://mirrors.tuna.tsinghua.edu.cn/fedora|g' \
    -i.bak /etc/yum.repos.d/fedora.repo /etc/yum.repos.d/fedora-updates.repo
"#
        .to_string(),
    };
    format!(
        r#"text
lang en_US.UTF-8
keyboard us
timezone UTC --utc
network --bootproto=dhcp --device=link --activate --onboot=on --hostname={hostname}
rootpw --lock
user --name=qtr --groups=wheel --lock
sshkey --username=qtr "{ssh_key}"
selinux --disabled
firewall --disabled
services --enabled=sshd,NetworkManager,chronyd,qemu-guest-agent --disabled=firewalld
firstboot --disable
eula --agreed
ignoredisk --only-use=vda
zerombr
clearpart --all --initlabel --drives=vda
autopart --type=btrfs
bootloader --location=mbr --append="selinux=0 console=ttyS0,115200n8"

%packages
@^custom-environment
qemu-guest-agent
openssh-server
sudo
curl
ca-certificates
%end

%post --erroronfail
install -d -m 0755 /var/lib/qtr
touch /var/lib/qtr/install-complete
printf 'qtr ALL=(ALL) NOPASSWD: ALL\n' > /etc/sudoers.d/qtr
chmod 0440 /etc/sudoers.d/qtr
systemctl enable sshd.service qemu-guest-agent.service
systemctl disable firewalld.service || true
systemctl mask firewalld.service
grubby --update-kernel=ALL --args="selinux=0 console=ttyS0,115200n8"
sed -i 's/^SELINUX=.*/SELINUX=disabled/' /etc/selinux/config
{mirror_post}%end

poweroff
"#
    )
}

fn validate_kickstart(path: &Path) -> Result<()> {
    let status = Command::new("ksvalidator")
        .arg("--firsterror")
        .arg(path)
        .status()
        .context("failed to run ksvalidator")?;
    if !status.success() {
        bail!("generated Fedora Kickstart failed validation");
    }
    Ok(())
}

fn detect_fedora_server_iso(path: &Path) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !file_name.starts_with("fedora-server-dvd-x86_64-") || !file_name.ends_with(".iso") {
        bail!("ISO filename does not identify Fedora Server DVD x86_64 media");
    }
    let output = Command::new("osinfo-detect")
        .args(["--type", "media"])
        .arg(path)
        .output()
        .context("failed to run osinfo-detect")?;
    if !output.status.success() {
        bail!("osinfo-detect could not identify ISO {}", path.display());
    }
    let detected = String::from_utf8_lossy(&output.stdout);
    validate_detected_fedora(&detected)?;
    let label = Command::new("blkid")
        .args(["-p", "-o", "value", "-s", "LABEL"])
        .arg(path)
        .output()
        .context("failed to read ISO volume label with blkid")?;
    if !label.status.success() {
        bail!("failed to read Fedora ISO volume label");
    }
    let label = String::from_utf8_lossy(&label.stdout);
    if !label.trim().starts_with("Fedora-S-dvd-x86_64-") {
        bail!(
            "unexpected Fedora Server DVD volume label: {}",
            label.trim()
        );
    }
    Ok(())
}

fn validate_detected_fedora(detected: &str) -> Result<()> {
    if !detected.to_ascii_lowercase().contains("fedora") {
        bail!(
            "ISO is not recognized as Fedora installation media: {}",
            detected.trim()
        );
    }
    Ok(())
}

fn ensure_domain_inactive(connect_uri: &str, name: &str) -> Result<()> {
    let conn = Connect::open(Some(connect_uri))?;
    let domain = Domain::lookup_by_name(&conn, name)?;
    if domain.is_active()? {
        bail!("installer exited but domain {name} is still active");
    }
    Ok(())
}

fn validate_ssh_key(value: &str) -> Result<&str> {
    let value = value.trim();
    if value.is_empty() || value.contains(['\n', '\r', '"']) {
        bail!("SSH public key must be one non-empty line without quotes");
    }
    let kind = value.split_whitespace().next().unwrap_or_default();
    if !(kind.starts_with("ssh-") || kind.starts_with("ecdsa-") || kind.starts_with("sk-")) {
        bail!("unsupported OpenSSH public key type {kind:?}");
    }
    if value.split_whitespace().count() < 2 {
        bail!("SSH public key is incomplete");
    }
    Ok(value)
}

fn validate_hostname(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 253
        || value.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                || !label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                || !label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
    {
        bail!("invalid hostname {value:?}");
    }
    Ok(())
}

fn canonical_file(path: &Path, label: &str) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("failed to resolve {label} {}", path.display()))?;
    if !path.is_file() {
        bail!("{label} is not a regular file: {}", path.display());
    }
    Ok(path)
}

fn canonical_destination(cwd: &Path, path: &Path, label: &str) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let file_name = path
        .file_name()
        .with_context(|| format!("{label} path has no file name: {}", path.display()))?;
    let parent = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .with_context(|| format!("{label} parent directory does not exist"))?;
    Ok(parent.join(file_name))
}

fn ensure_distinct_paths(paths: &[&Path]) -> Result<()> {
    for (index, left) in paths.iter().enumerate() {
        if paths[index + 1..].iter().any(|right| left == right) {
            bail!(
                "installer output paths must be distinct: {}",
                left.display()
            );
        }
    }
    Ok(())
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(contents)?;
    Ok(())
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".qtr-manifest-{}.tmp", Uuid::new_v4()));
    let result = fs::write(&temp, contents).and_then(|_| fs::hard_link(&temp, path));
    if result.is_ok() {
        let _ = fs::remove_file(&temp);
    }
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.with_context(|| format!("failed to write VM manifest {}", path.display()))
}

fn create_disk_atomic(path: &Path, size_bytes: u64) -> Result<FileIdentity> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp = parent.join(format!(".qtr-disk-{}.qcow2", Uuid::new_v4()));
    let result = disk::create_image(&temp, DiskFormat::Qcow2, &size_bytes.to_string())
        .and_then(|_| fs::hard_link(&temp, path).context("failed to publish disk image"));
    let _ = fs::remove_file(&temp);
    result?;
    file_identity(path)
}

fn file_identity(path: &Path) -> Result<FileIdentity> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect installer-owned file {}", path.display()))?;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn validate_ssh_key_file(path: &Path) -> Result<()> {
    let status = Command::new("ssh-keygen")
        .args(["-l", "-f"])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to run ssh-keygen")?;
    if !status.success() {
        bail!(
            "SSH public key failed ssh-keygen validation: {}",
            path.display()
        );
    }
    Ok(())
}

fn print_plan(plan: &InstallPlan) {
    println!("domain: {}", plan.args.name);
    println!("iso: {}", plan.iso.display());
    println!(
        "disk: {} ({} bytes)",
        plan.disk.display(),
        plan.disk_size_bytes
    );
    println!("manifest: {}", plan.output.display());
    println!("serial-log: {}", plan.serial_log.display());
    println!("install-log: {}", plan.install_log.display());
    println!("hostname: {}", plan.hostname);
    println!("mirror: {:?}", plan.args.mirror);
    println!(
        "ssh-key: configured ({} bytes)",
        plan.ssh_key_material.len()
    );
    println!("\n{}", plan.manifest_yaml);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_required_fedora_policy() {
        let kickstart = render_fedora_kickstart(
            "fedora-test",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITest qtr",
            FedoraMirror::Official,
        );
        for expected in [
            "autopart --type=btrfs",
            "@^custom-environment",
            "selinux --disabled",
            "firewall --disabled",
            "qemu-guest-agent",
            "systemctl mask firewalld.service",
            "grubby --update-kernel=ALL --args=\"selinux=0",
            "/var/lib/qtr/install-complete",
        ] {
            assert!(kickstart.contains(expected), "missing {expected}");
        }
        assert!(!kickstart.contains("mirrors.tuna"));
    }

    #[test]
    fn renders_tuna_repository_configuration() {
        let kickstart = render_fedora_kickstart(
            "fedora-test",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITest",
            FedoraMirror::Tuna,
        );
        assert!(kickstart.contains("https://mirrors.tuna.tsinghua.edu.cn/fedora"));
    }

    #[test]
    fn validates_ssh_keys_and_hostnames() {
        assert!(validate_ssh_key("ssh-ed25519 AAAA test").is_ok());
        assert!(validate_ssh_key("-----BEGIN PRIVATE KEY-----").is_err());
        assert!(validate_ssh_key("ssh-ed25519 AAAA\nssh-rsa BBBB").is_err());
        assert!(validate_hostname("fedora-44.example").is_ok());
        assert!(validate_hostname("-fedora").is_err());
        assert!(validate_hostname("fedora_test").is_err());
        assert!(validate_hostname("fedora..example").is_err());
        assert!(validate_hostname(&format!("{}.example", "a".repeat(64))).is_err());
    }

    #[test]
    fn accepts_generic_fedora_osinfo_detection() {
        assert!(validate_detected_fedora("Fedora Unknown (fedora-unknown)").is_ok());
        assert!(validate_detected_fedora("Ubuntu 24.04").is_err());
    }

    #[test]
    fn rejects_colliding_output_paths() {
        let path = Path::new("/tmp/same");
        assert!(ensure_distinct_paths(&[path, path]).is_err());
    }

    #[test]
    fn install_control_reports_cancellation() {
        let cancelled = Arc::new(AtomicBool::new(true));
        let control = InstallControl::new(cancelled, |_| {});
        assert_eq!(
            control.check().unwrap_err().to_string(),
            "installation cancelled"
        );
    }

    #[test]
    fn final_manifest_apply_preserves_domain_uuid() {
        let dir = std::env::temp_dir().join(format!("qtr-installer-test-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let disk = dir.join("root.qcow2");
        fs::write(&disk, b"test").unwrap();
        let name = format!("qtr-installer-{}", Uuid::new_v4());
        let args = VmInstallFedoraArgs {
            name: name.clone(),
            iso: dir.join("fedora.iso"),
            disk: disk.clone(),
            disk_size: "1GiB".to_string(),
            output: dir.join("vm.yaml"),
            serial_log: None,
            install_log: None,
            ssh_key: dir.join("id.pub"),
            memory_mib: 1024,
            vcpus: 1,
            network: "default".to_string(),
            hostname: None,
            mirror: FedoraMirror::Official,
            timeout_secs: 60,
            verify_timeout_secs: 60,
            connect_uri: "test:///default".to_string(),
            dry_run: false,
            keep_failed: false,
        };
        let manifest = fedora_manifest(&args, &disk, &dir.join("serial.log"));

        let expected_uuid = Uuid::new_v4().to_string();
        vm::define_new_by_manifest("test:///default", manifest.clone(), &expected_uuid).unwrap();
        let uuid = {
            let conn = Connect::open(Some("test:///default")).unwrap();
            let domain = Domain::lookup_by_name(&conn, &name).unwrap();
            domain.get_uuid_string().unwrap()
        };
        assert_eq!(uuid, expected_uuid);

        vm::apply_by_manifest("test:///default", manifest).unwrap();
        let conn = Connect::open(Some("test:///default")).unwrap();
        let domain = Domain::lookup_by_name(&conn, &name).unwrap();
        assert_eq!(domain.get_uuid_string().unwrap(), uuid);
        domain.undefine().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
