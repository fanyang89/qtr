use std::{
    ffi::OsString,
    fmt,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    net::IpAddr,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileTypeExt, OpenOptionsExt, PermissionsExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    cli_table,
    config::{
        DirectVmArgs, DirectVmCommand, DirectVmDefineArgs, DirectVmNameArgs, DirectVmStopArgs,
    },
};

const DIRECT_VM_SCHEMA_VERSION: u64 = 2;
const MIN_CLOUD_HYPERVISOR_USER_NETWORK_VERSION: u64 = 52;
const START_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
const FORCE_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const UNIX_SOCKET_PATH_LIMIT: usize = 107;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectVmManifest {
    schema_version: u64,
    name: String,
    firmware: PathBuf,
    cpus: u32,
    #[serde(rename = "memoryMiB")]
    memory_mib: u64,
    disks: Vec<DirectVmDisk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network: Option<DirectVmNetwork>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectVmManifestV1 {
    #[serde(rename = "schemaVersion")]
    _schema_version: u64,
    name: String,
    firmware: PathBuf,
    cpus: u32,
    #[serde(rename = "memoryMiB")]
    memory_mib: u64,
    disks: Vec<DirectVmDisk>,
    network: Option<DirectVmNetworkV1>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectVmNetworkV1 {
    tap: String,
    mac: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirectVmSchemaVersion {
    schema_version: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectVmDisk {
    path: PathBuf,
    #[serde(default)]
    readonly: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum DirectVmNetwork {
    User {
        #[serde(default = "generate_mac")]
        mac: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        forwards: Vec<DirectVmPortForward>,
    },
    Tap {
        tap: String,
        mac: String,
    },
}

impl DirectVmNetwork {
    fn is_user(&self) -> bool {
        matches!(self, Self::User { .. })
    }

    fn mac(&self) -> &str {
        match self {
            Self::User { mac, .. } | Self::Tap { mac, .. } => mac,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum DirectVmPortProtocol {
    Tcp,
    Udp,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectVmPortForward {
    protocol: DirectVmPortProtocol,
    #[serde(default = "default_forward_address")]
    host_address: IpAddr,
    host_port: u16,
    guest_port: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeStatus {
    Stopped,
    Running(u32),
    Degraded(u32),
    OrphanedNetwork(u32),
    Stale(u32),
    StaleNetwork(u32),
    Untracked,
    UntrackedNetwork,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessStatus {
    Stopped,
    Running(u32),
    Stale(u32),
    Untracked,
}

impl fmt::Display for RuntimeStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => formatter.write_str("stopped"),
            Self::Running(_) => formatter.write_str("running"),
            Self::Degraded(_) => formatter.write_str("degraded"),
            Self::OrphanedNetwork(_) => formatter.write_str("orphaned-network"),
            Self::Stale(_) => formatter.write_str("stale"),
            Self::StaleNetwork(_) => formatter.write_str("stale-network"),
            Self::Untracked => formatter.write_str("untracked"),
            Self::UntrackedNetwork => formatter.write_str("untracked-network"),
        }
    }
}

impl RuntimeStatus {
    fn pid(self) -> Option<u32> {
        match self {
            Self::Running(pid)
            | Self::Degraded(pid)
            | Self::OrphanedNetwork(pid)
            | Self::Stale(pid)
            | Self::StaleNetwork(pid) => Some(pid),
            Self::Stopped | Self::Untracked | Self::UntrackedNetwork => None,
        }
    }
}

struct DirectVmPaths {
    directory: PathBuf,
    manifest: PathBuf,
    pid: PathBuf,
    socket: PathBuf,
    network_pid: PathBuf,
    network_socket: PathBuf,
    serial_log: PathBuf,
    network_log: PathBuf,
    vmm_log: PathBuf,
}

impl DirectVmPaths {
    fn new(root: &Path, name: &str) -> Result<Self> {
        validate_name(name)?;
        let directory = root.join(name);
        Ok(Self {
            manifest: directory.join("manifest.yaml"),
            pid: directory.join("vmm.pid"),
            socket: directory.join("api.sock"),
            network_pid: directory.join("passt.pid"),
            network_socket: directory.join("network.sock"),
            serial_log: directory.join("serial.log"),
            network_log: directory.join("passt.log"),
            vmm_log: directory.join("vmm.log"),
            directory,
        })
    }
}

struct OperationLock {
    _file: File,
}

pub fn run(args: DirectVmArgs) -> Result<()> {
    let root = state_root(&args.state_dir)?;
    match args.command {
        DirectVmCommand::Define(command) => define(&root, command),
        DirectVmCommand::List => list(&root),
        DirectVmCommand::Show(command) => show(&root, command),
        DirectVmCommand::Start(command) => {
            start(&root, &args.cloud_hypervisor, &args.passt, command)
        }
        DirectVmCommand::Stop(command) => stop(&root, command),
        DirectVmCommand::Remove(command) => remove(&root, command),
    }
}

fn state_root(path: &Path) -> Result<PathBuf> {
    ensure_private_directory(path, "direct VM state directory")?;
    let root = path.canonicalize().with_context(|| {
        format!(
            "failed to resolve direct VM state directory {}",
            path.display()
        )
    })?;
    validate_config_path(&root, "state directory")?;
    ensure_private_directory(&root.join(".locks"), "direct VM lock directory")?;
    Ok(root)
}

fn ensure_private_directory(path: &Path, kind: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                bail!("{kind} {} is not a directory", path.display());
            }
            if metadata.permissions().mode() & 0o077 != 0 {
                bail!("{kind} {} must have mode 0700", path.display());
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            builder.mode(0o700).recursive(true);
            builder
                .create(path)
                .with_context(|| format!("failed to create {kind} {}", path.display()))
        }
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect {kind} {}", path.display()))
        }
    }
}

fn define(root: &Path, args: DirectVmDefineArgs) -> Result<()> {
    let contents = fs::read_to_string(&args.file)
        .with_context(|| format!("failed to read direct VM manifest {}", args.file.display()))?;
    let mut manifest = parse_manifest(&contents)
        .with_context(|| format!("failed to parse direct VM manifest {}", args.file.display()))?;
    let base = args
        .file
        .canonicalize()
        .with_context(|| {
            format!(
                "failed to resolve direct VM manifest {}",
                args.file.display()
            )
        })?
        .parent()
        .context("direct VM manifest has no parent directory")?
        .to_path_buf();
    normalize_manifest(&mut manifest, &base)?;
    let paths = DirectVmPaths::new(root, &manifest.name)?;
    validate_socket_path(&paths.socket)?;
    validate_socket_path(&paths.network_socket)?;
    let _lock = lock(root, &manifest.name)?;
    fs::create_dir(&paths.directory).with_context(|| {
        format!(
            "direct VM {} already exists or cannot be created",
            manifest.name
        )
    })?;
    fs::set_permissions(&paths.directory, fs::Permissions::from_mode(0o700))?;
    if let Err(error) = write_manifest(&paths.manifest, &manifest) {
        let _ = fs::remove_dir_all(&paths.directory);
        return Err(error);
    }
    eprintln!("[qtr] defined direct VM: {}", manifest.name);
    Ok(())
}

fn list(root: &Path) -> Result<()> {
    let mut rows = Vec::new();
    for entry in fs::read_dir(root).with_context(|| {
        format!(
            "failed to list direct VM state directory {}",
            root.display()
        )
    })? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(paths) = DirectVmPaths::new(root, &name) else {
            continue;
        };
        if !paths.manifest.is_file() {
            continue;
        }
        let manifest = read_manifest(&paths.manifest)?;
        let status = runtime_status(&paths, &manifest)?;
        rows.push((manifest.name, status));
    }
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    cli_table::print_table(
        &["NAME", "STATE", "PID"],
        rows.into_iter().map(|(name, status)| {
            let pid = status
                .pid()
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".to_string());
            vec![name, status.to_string(), pid]
        }),
    );
    Ok(())
}

fn show(root: &Path, args: DirectVmNameArgs) -> Result<()> {
    let paths = existing_paths(root, &args.name)?;
    let manifest = read_manifest(&paths.manifest)?;
    print!(
        "{}",
        serde_yaml::to_string(&manifest).context("failed to serialize direct VM manifest")?
    );
    Ok(())
}

fn start(root: &Path, cloud_hypervisor: &Path, passt: &Path, args: DirectVmNameArgs) -> Result<()> {
    let paths = existing_paths(root, &args.name)?;
    let _lock = lock(root, &args.name)?;
    let manifest = read_manifest(&paths.manifest)?;
    match runtime_status(&paths, &manifest)? {
        RuntimeStatus::Running(pid) | RuntimeStatus::Degraded(pid) => {
            bail!("direct VM {} is already running as PID {pid}", args.name)
        }
        RuntimeStatus::OrphanedNetwork(pid) => bail!(
            "direct VM {} has an orphaned passt process as PID {pid}; stop it before starting",
            args.name
        ),
        RuntimeStatus::Stale(pid) => bail!(
            "direct VM {} has PID {pid} owned by another process; remove {} after verifying the process",
            args.name,
            paths.pid.display()
        ),
        RuntimeStatus::StaleNetwork(pid) => bail!(
            "direct VM {} has passt PID {pid} owned by another process; inspect {} before starting",
            args.name,
            paths.network_pid.display()
        ),
        RuntimeStatus::Untracked => bail!(
            "direct VM {} has an active API socket without tracked process metadata",
            args.name
        ),
        RuntimeStatus::UntrackedNetwork => bail!(
            "direct VM {} has a passt socket without tracked process metadata",
            args.name
        ),
        RuntimeStatus::Stopped => {}
    }
    remove_stale_runtime(&paths)?;
    validate_runtime_resources(&manifest)?;
    let vmm_log = open_append_regular(&paths.vmm_log)?;
    drop(open_append_regular(&paths.serial_log)?);
    let stderr = vmm_log
        .try_clone()
        .context("failed to clone VMM log handle")?;
    let mut network_child = if manifest
        .network
        .as_ref()
        .is_some_and(DirectVmNetwork::is_user)
    {
        validate_user_network_prerequisites(cloud_hypervisor, passt)?;
        Some(start_passt(passt, &manifest, &paths)?)
    } else {
        None
    };
    let mut child = match Command::new(cloud_hypervisor)
        .args(cloud_hypervisor_args(&manifest, &paths))
        .stdin(Stdio::null())
        .stdout(Stdio::from(vmm_log))
        .stderr(Stdio::from(stderr))
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            stop_spawned_network(network_child.as_mut())?;
            remove_stale_runtime(&paths)?;
            return Err(error).with_context(|| {
                format!(
                    "failed to start Cloud Hypervisor using {}",
                    cloud_hypervisor.display()
                )
            });
        }
    };

    if let Err(error) = write_pid(&paths.pid, child.id()) {
        stop_spawned_child(&mut child, &paths.socket)?;
        stop_spawned_network(network_child.as_mut())?;
        remove_stale_runtime(&paths)?;
        return Err(error);
    }

    let deadline = Instant::now() + START_TIMEOUT;
    let mut last_api_error = None;
    loop {
        if let Some(network) = network_child.as_mut() {
            match network.try_wait() {
                Ok(Some(status)) => {
                    stop_spawned_child(&mut child, &paths.socket)?;
                    remove_stale_runtime(&paths)?;
                    bail!(
                        "passt exited with {status} while starting direct VM {}; see {}",
                        args.name,
                        paths.network_log.display()
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    stop_spawned_child(&mut child, &paths.socket)?;
                    stop_spawned_network(Some(network))?;
                    remove_stale_runtime(&paths)?;
                    return Err(error).context("failed to query passt process");
                }
            }
        }
        let socket_ready = match socket_ready(&paths.socket) {
            Ok(ready) => ready,
            Err(error) => {
                stop_spawned_child(&mut child, &paths.socket)?;
                stop_spawned_network(network_child.as_mut())?;
                remove_stale_runtime(&paths)?;
                return Err(error);
            }
        };
        if socket_ready {
            match vm_is_running(&paths.socket) {
                Ok(true) => {
                    if let Some(network) = network_child.as_mut() {
                        match network.try_wait() {
                            Ok(None) => {}
                            Ok(Some(status)) => {
                                stop_spawned_child(&mut child, &paths.socket)?;
                                remove_stale_runtime(&paths)?;
                                bail!(
                                    "passt exited with {status} while starting direct VM {}; see {}",
                                    args.name,
                                    paths.network_log.display()
                                );
                            }
                            Err(error) => {
                                stop_spawned_child(&mut child, &paths.socket)?;
                                stop_spawned_network(Some(network))?;
                                remove_stale_runtime(&paths)?;
                                return Err(error).context("failed to query passt process");
                            }
                        }
                    }
                    eprintln!(
                        "[qtr] started direct VM {} as PID {}",
                        args.name,
                        child.id()
                    );
                    return Ok(());
                }
                Ok(false) => {}
                Err(error) => last_api_error = Some(error),
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                stop_spawned_network(network_child.as_mut())?;
                remove_stale_runtime(&paths)?;
                bail!(
                    "Cloud Hypervisor exited with {status} before creating {}; see {}",
                    paths.socket.display(),
                    paths.vmm_log.display()
                );
            }
            Ok(None) => {}
            Err(error) => {
                stop_spawned_child(&mut child, &paths.socket)?;
                stop_spawned_network(network_child.as_mut())?;
                remove_stale_runtime(&paths)?;
                return Err(error).context("failed to query Cloud Hypervisor process");
            }
        }
        if Instant::now() >= deadline {
            stop_spawned_child(&mut child, &paths.socket)?;
            stop_spawned_network(network_child.as_mut())?;
            remove_stale_runtime(&paths)?;
            let api_error = last_api_error
                .map(|error| format!("; last API error: {error:#}"))
                .unwrap_or_default();
            bail!(
                "timed out waiting for Cloud Hypervisor API socket {}{api_error}; see {}",
                paths.socket.display(),
                paths.vmm_log.display()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn start_passt(
    executable: &Path,
    manifest: &DirectVmManifest,
    paths: &DirectVmPaths,
) -> Result<Child> {
    drop(open_append_regular(&paths.network_log)?);
    let dns_servers = discover_dns_servers()?;
    let mut child = Command::new(executable)
        .args(passt_args(manifest, paths, &dns_servers))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to start passt using {}", executable.display()))?;

    if let Err(error) = write_pid(&paths.network_pid, child.id()) {
        stop_spawned_network(Some(&mut child))?;
        remove_network_runtime(paths)?;
        return Err(error);
    }

    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match socket_ready(&paths.network_socket) {
            Ok(true) => match child.try_wait() {
                Ok(None) => return Ok(child),
                Ok(Some(status)) => {
                    remove_network_runtime(paths)?;
                    bail!(
                        "passt exited with {status} after creating {}; see {}",
                        paths.network_socket.display(),
                        paths.network_log.display()
                    );
                }
                Err(error) => {
                    stop_spawned_network(Some(&mut child))?;
                    remove_network_runtime(paths)?;
                    return Err(error).context("failed to query passt process");
                }
            },
            Ok(false) => {}
            Err(error) => {
                stop_spawned_network(Some(&mut child))?;
                remove_network_runtime(paths)?;
                return Err(error);
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                remove_network_runtime(paths)?;
                bail!(
                    "passt exited with {status} before creating {}; see {}",
                    paths.network_socket.display(),
                    paths.network_log.display()
                );
            }
            Ok(None) => {}
            Err(error) => {
                stop_spawned_network(Some(&mut child))?;
                remove_network_runtime(paths)?;
                return Err(error).context("failed to query passt process");
            }
        }
        if Instant::now() >= deadline {
            stop_spawned_network(Some(&mut child))?;
            remove_network_runtime(paths)?;
            bail!(
                "timed out waiting for passt socket {}; see {}",
                paths.network_socket.display(),
                paths.network_log.display()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn stop_spawned_child(child: &mut Child, socket: &Path) -> Result<()> {
    if socket_ready(socket).unwrap_or(false) {
        let _ = api_request(socket, "PUT", "vmm.shutdown");
        let deadline = Instant::now() + FORCE_STOP_TIMEOUT;
        while Instant::now() < deadline {
            if child
                .try_wait()
                .context("failed to query spawned Cloud Hypervisor process")?
                .is_some()
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    kill_spawned_child(child, "Cloud Hypervisor")
}

fn stop_spawned_network(child: Option<&mut Child>) -> Result<()> {
    let Some(child) = child else {
        return Ok(());
    };
    kill_spawned_child(child, "passt")
}

fn kill_spawned_child(child: &mut Child, name: &str) -> Result<()> {
    if child
        .try_wait()
        .with_context(|| format!("failed to query spawned {name} process"))?
        .is_some()
    {
        return Ok(());
    }
    if let Err(error) = child.kill() {
        if child
            .try_wait()
            .with_context(|| format!("failed to query spawned {name} process"))?
            .is_some()
        {
            return Ok(());
        }
        return Err(error).with_context(|| format!("failed to kill spawned {name} process"));
    }
    child
        .wait()
        .with_context(|| format!("failed to reap spawned {name} process"))?;
    Ok(())
}

fn remove_network_runtime(paths: &DirectVmPaths) -> Result<()> {
    remove_inactive_socket(&paths.network_socket)?;
    remove_file_if_exists(&paths.network_pid)?;
    Ok(())
}

fn stop(root: &Path, args: DirectVmStopArgs) -> Result<()> {
    let paths = existing_paths(root, &args.name)?;
    let _lock = lock(root, &args.name)?;
    let manifest = read_manifest(&paths.manifest)?;
    let status = runtime_status(&paths, &manifest)?;
    if let RuntimeStatus::OrphanedNetwork(pid) = status {
        terminate_passt(pid, &paths)?;
        remove_stale_runtime(&paths)?;
        eprintln!(
            "[qtr] stopped orphaned network for direct VM: {}",
            args.name
        );
        return Ok(());
    }
    let pid = match status {
        RuntimeStatus::Running(pid) | RuntimeStatus::Degraded(pid) => pid,
        _ => {
            if matches!(status, RuntimeStatus::Stopped) {
                remove_stale_runtime(&paths)?;
                eprintln!("[qtr] direct VM is already stopped: {}", args.name);
                return Ok(());
            }
            match status {
                RuntimeStatus::Stale(pid) => bail!(
                    "refusing to stop PID {pid}; it no longer belongs to direct VM {}",
                    args.name
                ),
                RuntimeStatus::StaleNetwork(pid) => bail!(
                    "refusing to stop passt PID {pid}; it no longer belongs to direct VM {}",
                    args.name
                ),
                RuntimeStatus::Untracked => bail!(
                    "direct VM {} has an active API socket without tracked process metadata",
                    args.name
                ),
                RuntimeStatus::UntrackedNetwork => bail!(
                    "direct VM {} has a passt socket without tracked process metadata",
                    args.name
                ),
                RuntimeStatus::Stopped
                | RuntimeStatus::Running(_)
                | RuntimeStatus::Degraded(_)
                | RuntimeStatus::OrphanedNetwork(_) => unreachable!(),
            }
        }
    };

    let (endpoint, timeout) = if args.force {
        ("vmm.shutdown", FORCE_STOP_TIMEOUT)
    } else {
        ("vm.power-button", STOP_TIMEOUT)
    };
    api_request(&paths.socket, "PUT", endpoint)?;
    let deadline = Instant::now() + timeout;
    while process_matches(pid, &paths.socket)? && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    if process_matches(pid, &paths.socket)? {
        if args.force {
            bail!("Cloud Hypervisor PID {pid} did not exit after VMM shutdown");
        }
        bail!("guest did not shut down before the timeout; retry with --force");
    }
    wait_for_network_shutdown(&paths, &manifest)?;
    remove_stale_runtime(&paths)?;
    eprintln!("[qtr] stopped direct VM: {}", args.name);
    Ok(())
}

fn remove(root: &Path, args: DirectVmNameArgs) -> Result<()> {
    let paths = existing_paths(root, &args.name)?;
    let _lock = lock(root, &args.name)?;
    let manifest = read_manifest(&paths.manifest)?;
    match runtime_status(&paths, &manifest)? {
        RuntimeStatus::Running(pid) | RuntimeStatus::Degraded(pid) => {
            bail!(
                "direct VM {} is running as PID {pid}; stop it first",
                args.name
            )
        }
        RuntimeStatus::OrphanedNetwork(pid) => bail!(
            "direct VM {} has an orphaned passt process as PID {pid}; stop it before removal",
            args.name
        ),
        RuntimeStatus::Stale(pid) => bail!(
            "direct VM {} has PID {pid} owned by another process; inspect {} before removal",
            args.name,
            paths.pid.display()
        ),
        RuntimeStatus::StaleNetwork(pid) => bail!(
            "direct VM {} has passt PID {pid} owned by another process; inspect {} before removal",
            args.name,
            paths.network_pid.display()
        ),
        RuntimeStatus::Untracked => bail!(
            "direct VM {} has an active API socket without tracked process metadata",
            args.name
        ),
        RuntimeStatus::UntrackedNetwork => bail!(
            "direct VM {} has a passt socket without tracked process metadata",
            args.name
        ),
        RuntimeStatus::Stopped => {}
    }
    fs::remove_dir_all(&paths.directory)
        .with_context(|| format!("failed to remove direct VM {}", args.name))?;
    eprintln!("[qtr] removed direct VM: {}", args.name);
    Ok(())
}

fn existing_paths(root: &Path, name: &str) -> Result<DirectVmPaths> {
    let paths = DirectVmPaths::new(root, name)?;
    let metadata = fs::symlink_metadata(&paths.directory)
        .with_context(|| format!("direct VM {name} was not found"))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "direct VM state path {} is not a directory",
            paths.directory.display()
        );
    }
    if !paths.manifest.is_file() {
        bail!("direct VM {name} has no manifest");
    }
    Ok(paths)
}

fn read_manifest(path: &Path) -> Result<DirectVmManifest> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect direct VM manifest {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!(
            "direct VM manifest {} is not a regular file",
            path.display()
        );
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read direct VM manifest {}", path.display()))?;
    let manifest = parse_manifest(&contents)
        .with_context(|| format!("failed to parse direct VM manifest {}", path.display()))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn parse_manifest(contents: &str) -> Result<DirectVmManifest> {
    let version: DirectVmSchemaVersion =
        serde_yaml::from_str(contents).context("failed to read direct VM schemaVersion")?;
    let manifest = match version.schema_version {
        1 => {
            let manifest: DirectVmManifestV1 = serde_yaml::from_str(contents)?;
            DirectVmManifest {
                schema_version: DIRECT_VM_SCHEMA_VERSION,
                name: manifest.name,
                firmware: manifest.firmware,
                cpus: manifest.cpus,
                memory_mib: manifest.memory_mib,
                disks: manifest.disks,
                network: manifest.network.map(|network| DirectVmNetwork::Tap {
                    tap: network.tap,
                    mac: network.mac,
                }),
            }
        }
        DIRECT_VM_SCHEMA_VERSION => serde_yaml::from_str(contents)?,
        version => bail!(
            "unsupported direct VM schemaVersion {version}; expected 1 or {DIRECT_VM_SCHEMA_VERSION}"
        ),
    };
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn write_manifest(path: &Path, manifest: &DirectVmManifest) -> Result<()> {
    let contents =
        serde_yaml::to_string(manifest).context("failed to serialize direct VM manifest")?;
    let temporary = path.with_extension(format!("yaml.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| format!("failed to create {}", temporary.display()))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, path)
        .with_context(|| format!("failed to publish direct VM manifest {}", path.display()))
}

fn normalize_manifest(manifest: &mut DirectVmManifest, base: &Path) -> Result<()> {
    validate_manifest(manifest)?;
    manifest.firmware = canonical_file(base, &manifest.firmware, "firmware")?;
    for disk in &mut manifest.disks {
        disk.path = canonical_file(base, &disk.path, "disk")?;
    }
    Ok(())
}

fn validate_manifest(manifest: &DirectVmManifest) -> Result<()> {
    if manifest.schema_version != DIRECT_VM_SCHEMA_VERSION {
        bail!(
            "unsupported direct VM schemaVersion {}; expected {}",
            manifest.schema_version,
            DIRECT_VM_SCHEMA_VERSION
        );
    }
    validate_name(&manifest.name)?;
    if manifest.cpus == 0 {
        bail!("direct VM cpus must be greater than zero");
    }
    if manifest.memory_mib == 0 {
        bail!("direct VM memoryMiB must be greater than zero");
    }
    if manifest.disks.is_empty() {
        bail!("direct VM must contain at least one disk");
    }
    if let Some(network) = &manifest.network {
        if !valid_mac(network.mac()) {
            bail!("direct VM network MAC address is invalid");
        }
        match network {
            DirectVmNetwork::User { forwards, .. } => {
                if !valid_unicast_mac(network.mac()) {
                    bail!("direct VM user network MAC address must be unicast");
                }
                let mut bindings = Vec::new();
                for forward in forwards {
                    if forward.host_port == 0 || forward.guest_port == 0 {
                        bail!("direct VM forwarded ports must be greater than zero");
                    }
                    if matches!(
                        forward.host_address,
                        IpAddr::V6(address) if address.to_ipv4_mapped().is_some()
                    ) {
                        bail!("direct VM forwarding hostAddress must not be IPv4-mapped IPv6");
                    }
                    if bindings.iter().any(|(protocol, address, port)| {
                        *protocol == forward.protocol
                            && *port == forward.host_port
                            && same_ip_family(*address, forward.host_address)
                            && (address.is_unspecified()
                                || forward.host_address.is_unspecified()
                                || *address == forward.host_address)
                    }) {
                        bail!(
                            "conflicting direct VM {:?} forwarding binding {}:{}",
                            forward.protocol,
                            forward.host_address,
                            forward.host_port
                        );
                    }
                    bindings.push((forward.protocol, forward.host_address, forward.host_port));
                }
            }
            DirectVmNetwork::Tap { tap, .. } => {
                if tap.is_empty()
                    || !tap.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
                {
                    bail!("direct VM TAP name contains unsupported characters");
                }
            }
        }
    }
    Ok(())
}

fn validate_runtime_resources(manifest: &DirectVmManifest) -> Result<()> {
    if !manifest.firmware.is_file() {
        bail!(
            "firmware {} is not a regular file",
            manifest.firmware.display()
        );
    }
    for disk in &manifest.disks {
        if !disk.path.is_file() {
            bail!("disk {} is not a regular file", disk.path.display());
        }
    }
    if let Some(DirectVmNetwork::Tap { tap, .. }) = &manifest.network {
        let path = Path::new("/sys/class/net").join(tap);
        if !path.exists() {
            bail!("pre-created TAP interface {tap} does not exist");
        }
        if !path.join("tun_flags").exists() {
            bail!("network interface {tap} is not a TAP device");
        }
    }
    Ok(())
}

fn canonical_file(base: &Path, path: &Path, kind: &str) -> Result<PathBuf> {
    let source = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let path = source
        .canonicalize()
        .with_context(|| format!("failed to resolve {kind} {}", path.display()))?;
    if !path.is_file() {
        bail!("{kind} {} is not a regular file", path.display());
    }
    validate_config_path(&path, kind)?;
    Ok(path)
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || matches!(name, "." | "..")
        || !name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!(
            "direct VM name must contain 1-64 letters, numbers, '.', '_' or '-' and start and end with a letter or number"
        );
    }
    Ok(())
}

fn valid_mac(mac: &str) -> bool {
    let parts = mac.split(':').collect::<Vec<_>>();
    parts.len() == 6
        && parts
            .iter()
            .all(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn valid_unicast_mac(mac: &str) -> bool {
    valid_mac(mac)
        && u8::from_str_radix(mac.split(':').next().unwrap_or_default(), 16)
            .is_ok_and(|first| first & 1 == 0)
}

fn generate_mac() -> String {
    let bytes = Uuid::new_v4().into_bytes();
    format!(
        "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]
    )
}

fn default_forward_address() -> IpAddr {
    IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

fn same_ip_family(left: IpAddr, right: IpAddr) -> bool {
    matches!(
        (left, right),
        (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
    )
}

fn validate_config_path<'a>(path: &'a Path, kind: &str) -> Result<&'a str> {
    let value = path
        .to_str()
        .with_context(|| format!("{kind} path must be valid UTF-8: {}", path.display()))?;
    if value.contains([',', '\n', '\r']) {
        bail!("{kind} path contains characters unsupported by Cloud Hypervisor: {value:?}");
    }
    Ok(value)
}

fn validate_socket_path(path: &Path) -> Result<()> {
    if path.as_os_str().as_bytes().len() > UNIX_SOCKET_PATH_LIMIT {
        bail!(
            "Cloud Hypervisor API socket path exceeds {UNIX_SOCKET_PATH_LIMIT} bytes: {}",
            path.display()
        );
    }
    Ok(())
}

fn cloud_hypervisor_args(manifest: &DirectVmManifest, paths: &DirectVmPaths) -> Vec<OsString> {
    let shared_memory = manifest
        .network
        .as_ref()
        .is_some_and(DirectVmNetwork::is_user);
    let mut args = vec![
        "--api-socket".into(),
        paths.socket.as_os_str().into(),
        "--firmware".into(),
        manifest.firmware.as_os_str().into(),
        "--cpus".into(),
        format!("boot={}", manifest.cpus).into(),
        "--memory".into(),
        format!(
            "size={}M{}",
            manifest.memory_mib,
            if shared_memory { ",shared=on" } else { "" }
        )
        .into(),
    ];
    for disk in &manifest.disks {
        args.push("--disk".into());
        let mut value = format!(
            "path={}",
            validate_config_path(&disk.path, "disk").expect("validated direct VM disk path")
        );
        if disk.readonly {
            value.push_str(",readonly=on");
        }
        args.push(value.into());
    }
    if let Some(network) = &manifest.network {
        args.push("--net".into());
        args.push(match network {
            DirectVmNetwork::User { mac, .. } => format!(
                "vhost_user=on,socket={},vhost_mode=client,mac={mac}",
                validate_config_path(&paths.network_socket, "passt socket")
                    .expect("validated direct VM state path")
            )
            .into(),
            DirectVmNetwork::Tap { tap, mac } => format!("tap={tap},mac={mac}").into(),
        });
    }
    args.extend([
        "--serial".into(),
        format!(
            "file={}",
            validate_config_path(&paths.serial_log, "serial log")
                .expect("validated direct VM state path")
        )
        .into(),
        "--console".into(),
        "off".into(),
    ]);
    args
}

fn passt_args(
    manifest: &DirectVmManifest,
    paths: &DirectVmPaths,
    dns_servers: &[IpAddr],
) -> Vec<OsString> {
    let Some(DirectVmNetwork::User { forwards, .. }) = &manifest.network else {
        return Vec::new();
    };
    let mut args = vec![
        "--foreground".into(),
        "--one-off".into(),
        "--vhost-user".into(),
        "--socket".into(),
        paths.network_socket.as_os_str().into(),
        "--log-file".into(),
        paths.network_log.as_os_str().into(),
        "--log-size".into(),
        "1048576".into(),
        "--repair-path".into(),
        "none".into(),
        "--map-host-loopback".into(),
        "none".into(),
        "--hostname".into(),
        manifest.name.clone().into(),
    ];
    for server in dns_servers {
        args.push("--dns".into());
        args.push(server.to_string().into());
    }
    for protocol in [DirectVmPortProtocol::Tcp, DirectVmPortProtocol::Udp] {
        let matching = forwards
            .iter()
            .filter(|forward| forward.protocol == protocol)
            .collect::<Vec<_>>();
        let option = match protocol {
            DirectVmPortProtocol::Tcp => "--tcp-ports",
            DirectVmPortProtocol::Udp => "--udp-ports",
        };
        if matching.is_empty() {
            args.push(option.into());
            args.push("none".into());
            continue;
        }
        for forward in matching {
            args.push(option.into());
            args.push(
                format!(
                    "{}/{}:{}",
                    forward.host_address, forward.host_port, forward.guest_port
                )
                .into(),
            );
        }
    }
    args
}

fn discover_dns_servers() -> Result<Vec<IpAddr>> {
    let mut servers = read_dns_servers(Path::new("/etc/resolv.conf"))?;
    if servers.is_empty() {
        servers = read_dns_servers(Path::new("/run/systemd/resolve/resolv.conf"))?;
    }
    if servers.is_empty() {
        bail!(
            "user networking requires a non-loopback DNS server in /etc/resolv.conf or /run/systemd/resolve/resolv.conf"
        );
    }
    Ok(servers)
}

fn read_dns_servers(path: &Path) -> Result<Vec<IpAddr>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let mut servers = Vec::new();
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("nameserver") {
            continue;
        }
        let Some(server) = fields.next().and_then(|value| value.parse::<IpAddr>().ok()) else {
            continue;
        };
        if !server.is_loopback() && !server.is_unspecified() && !servers.contains(&server) {
            servers.push(server);
        }
    }
    Ok(servers)
}

fn validate_user_network_prerequisites(cloud_hypervisor: &Path, passt: &Path) -> Result<()> {
    let output = Command::new(cloud_hypervisor)
        .arg("--version")
        .output()
        .with_context(|| {
            format!(
                "failed to query Cloud Hypervisor version using {}",
                cloud_hypervisor.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "Cloud Hypervisor version query failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let version = String::from_utf8(output.stdout)
        .context("Cloud Hypervisor returned a non-UTF-8 version")?;
    let major = parse_cloud_hypervisor_major(&version)?;
    if major < MIN_CLOUD_HYPERVISOR_USER_NETWORK_VERSION {
        bail!(
            "user networking requires Cloud Hypervisor {} or newer; found {version:?}",
            MIN_CLOUD_HYPERVISOR_USER_NETWORK_VERSION
        );
    }

    let output = Command::new(passt)
        .args(["--vhost-user", "--print-capabilities"])
        .output()
        .with_context(|| {
            format!(
                "failed to query passt capabilities using {}",
                passt.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "passt capability query failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let capabilities = parse_passt_capabilities(&output.stdout, &output.stderr)?;
    if capabilities.get("type").and_then(|value| value.as_str()) != Some("net") {
        bail!("passt does not report vhost-user network capability");
    }
    Ok(())
}

fn parse_passt_capabilities(stdout: &[u8], stderr: &[u8]) -> Result<serde_json::Value> {
    let output = if stdout.is_empty() { stderr } else { stdout };
    serde_json::from_slice(output).context("failed to parse passt vhost-user capabilities")
}

fn parse_cloud_hypervisor_major(version: &str) -> Result<u64> {
    version
        .split_whitespace()
        .find_map(|part| part.strip_prefix('v'))
        .and_then(|version| version.split('.').next())
        .context("Cloud Hypervisor version output does not contain a v-prefixed version")?
        .parse()
        .context("failed to parse Cloud Hypervisor major version")
}

fn lock(root: &Path, name: &str) -> Result<OperationLock> {
    validate_name(name)?;
    let path = root.join(".locks").join(format!("{name}.lock"));
    let file = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&path)
        .with_context(|| format!("failed to open operation lock {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("operation lock {} is not a regular file", path.display());
    }
    file.try_lock()
        .with_context(|| format!("another operation is active for direct VM {name}"))?;
    Ok(OperationLock { _file: file })
}

fn runtime_status(paths: &DirectVmPaths, manifest: &DirectVmManifest) -> Result<RuntimeStatus> {
    let vmm = vmm_process_status(paths)?;
    if !manifest
        .network
        .as_ref()
        .is_some_and(DirectVmNetwork::is_user)
    {
        return Ok(match vmm {
            ProcessStatus::Stopped => RuntimeStatus::Stopped,
            ProcessStatus::Running(pid) => RuntimeStatus::Running(pid),
            ProcessStatus::Stale(pid) => RuntimeStatus::Stale(pid),
            ProcessStatus::Untracked => RuntimeStatus::Untracked,
        });
    }

    let network = network_process_status(paths)?;
    Ok(match (vmm, network) {
        (ProcessStatus::Running(pid), ProcessStatus::Running(_)) => RuntimeStatus::Running(pid),
        (ProcessStatus::Running(pid), _) => RuntimeStatus::Degraded(pid),
        (ProcessStatus::Stopped, ProcessStatus::Running(pid)) => {
            RuntimeStatus::OrphanedNetwork(pid)
        }
        (ProcessStatus::Stopped, ProcessStatus::Stale(pid)) => RuntimeStatus::StaleNetwork(pid),
        (ProcessStatus::Stopped, ProcessStatus::Untracked) => RuntimeStatus::UntrackedNetwork,
        (ProcessStatus::Stopped, ProcessStatus::Stopped) => RuntimeStatus::Stopped,
        (ProcessStatus::Stale(pid), _) => RuntimeStatus::Stale(pid),
        (ProcessStatus::Untracked, _) => RuntimeStatus::Untracked,
    })
}

fn vmm_process_status(paths: &DirectVmPaths) -> Result<ProcessStatus> {
    let Some(pid) = read_pid(&paths.pid)? else {
        return untracked_vmm_status(paths);
    };
    if !process_is_alive(pid)? {
        return untracked_vmm_status(paths);
    }
    let socket = unix_socket_state(&paths.socket)?;
    if process_matches(pid, &paths.socket)? && process_owns_unix_socket(pid, &socket)? == Some(true)
    {
        Ok(ProcessStatus::Running(pid))
    } else {
        Ok(ProcessStatus::Stale(pid))
    }
}

fn untracked_vmm_status(paths: &DirectVmPaths) -> Result<ProcessStatus> {
    if unix_socket_state(&paths.socket)?.active() {
        return Ok(ProcessStatus::Untracked);
    }
    socket_ready(&paths.socket)?;
    Ok(ProcessStatus::Stopped)
}

fn network_process_status(paths: &DirectVmPaths) -> Result<ProcessStatus> {
    let Some(pid) = read_pid(&paths.network_pid)? else {
        return untracked_network_status(paths);
    };
    if !process_is_alive(pid)? {
        return untracked_network_status(paths);
    }
    let socket = unix_socket_state(&paths.network_socket)?;
    if socket.active()
        && passt_process_matches(pid, &paths.network_socket)?
        && process_owns_unix_socket(pid, &socket)? != Some(false)
    {
        Ok(ProcessStatus::Running(pid))
    } else {
        Ok(ProcessStatus::Stale(pid))
    }
}

fn untracked_network_status(paths: &DirectVmPaths) -> Result<ProcessStatus> {
    let socket = unix_socket_state(&paths.network_socket)?;
    if !socket.active() {
        socket_ready(&paths.network_socket)?;
        return Ok(ProcessStatus::Stopped);
    }
    if let Some(pid) = find_passt_process(&paths.network_socket, &socket)? {
        return Ok(ProcessStatus::Running(pid));
    }
    Ok(ProcessStatus::Untracked)
}

fn read_pid(path: &Path) -> Result<Option<u32>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let pid = contents
        .trim()
        .parse::<u32>()
        .with_context(|| format!("invalid PID file {}", path.display()))?;
    Ok(Some(pid))
}

fn write_pid(path: &Path, pid: u32) -> Result<()> {
    let temporary = path.with_extension(format!("pid.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| format!("failed to create PID file {}", temporary.display()))?;
    writeln!(file, "{pid}")?;
    file.sync_all()?;
    fs::rename(&temporary, path)
        .with_context(|| format!("failed to publish PID file {}", path.display()))
}

fn process_matches(pid: u32, socket: &Path) -> Result<bool> {
    let Some(cmdline) = read_process_cmdline(pid)? else {
        return Ok(false);
    };
    let socket = socket.as_os_str().as_encoded_bytes();
    Ok(cmdline
        .split(|byte| *byte == 0)
        .any(|argument| argument == socket))
}

fn process_is_alive(pid: u32) -> Result<bool> {
    let path = Path::new("/proc").join(pid.to_string()).join("stat");
    let stat = match fs::read(&path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect process state for PID {pid}"));
        }
    };
    let close = stat
        .iter()
        .rposition(|byte| *byte == b')')
        .context("invalid process stat format")?;
    Ok(!matches!(stat.get(close + 2), Some(b'Z' | b'X')))
}

fn passt_process_matches(pid: u32, socket: &Path) -> Result<bool> {
    let Some(cmdline) = read_process_cmdline(pid)? else {
        return Ok(false);
    };
    Ok(cmdline_matches_passt(&cmdline, socket))
}

fn read_process_cmdline(pid: u32) -> Result<Option<Vec<u8>>> {
    let path = Path::new("/proc").join(pid.to_string()).join("cmdline");
    match fs::read(&path) {
        Ok(cmdline) => Ok(Some(cmdline)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect process command line for PID {pid}")),
    }
}

fn cmdline_matches_passt(cmdline: &[u8], socket: &Path) -> bool {
    let arguments = cmdline
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .collect::<Vec<_>>();
    let socket = socket.as_os_str().as_encoded_bytes();
    arguments
        .iter()
        .any(|argument| *argument == b"--vhost-user")
        && arguments.windows(2).any(|arguments| {
            matches!(arguments[0], b"--socket" | b"--socket-path" | b"-s") && arguments[1] == socket
        })
}

struct UnixSocketState {
    inodes: Vec<u64>,
}

impl UnixSocketState {
    fn active(&self) -> bool {
        !self.inodes.is_empty()
    }
}

fn unix_socket_state(path: &Path) -> Result<UnixSocketState> {
    let table = fs::read("/proc/net/unix").context("failed to inspect UNIX sockets")?;
    let path = path.as_os_str().as_encoded_bytes();
    let mut inodes = Vec::new();
    for line in table.split(|byte| *byte == b'\n').skip(1) {
        let mut offset = 0;
        let mut inode = None;
        for field in 0..7 {
            let Some(value) = next_ascii_field(line, &mut offset) else {
                break;
            };
            if field == 6 {
                inode = std::str::from_utf8(value)
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok());
            }
        }
        while line.get(offset).is_some_and(u8::is_ascii_whitespace) {
            offset += 1;
        }
        if line.get(offset..) == Some(path)
            && let Some(inode) = inode
        {
            inodes.push(inode);
        }
    }
    Ok(UnixSocketState { inodes })
}

fn process_owns_unix_socket(pid: u32, socket: &UnixSocketState) -> Result<Option<bool>> {
    let path = Path::new("/proc").join(pid.to_string()).join("fd");
    let descriptors = match fs::read_dir(&path) {
        Ok(descriptors) => descriptors,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Some(false)),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect file descriptors for PID {pid}"));
        }
    };
    Ok(Some(descriptors.filter_map(Result::ok).any(|descriptor| {
        fs::read_link(descriptor.path())
            .ok()
            .and_then(|target| socket_inode(&target))
            .is_some_and(|inode| socket.inodes.contains(&inode))
    })))
}

fn find_passt_process(socket_path: &Path, socket: &UnixSocketState) -> Result<Option<u32>> {
    let mut candidate = None;
    for entry in fs::read_dir("/proc").context("failed to inspect running processes")? {
        let Ok(entry) = entry else {
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if !cmdline_matches_passt(&cmdline, socket_path)
            || process_owns_unix_socket(pid, socket)? == Some(false)
        {
            continue;
        }
        if candidate.replace(pid).is_some() {
            return Ok(None);
        }
    }
    Ok(candidate)
}

fn next_ascii_field<'a>(line: &'a [u8], offset: &mut usize) -> Option<&'a [u8]> {
    while line.get(*offset).is_some_and(u8::is_ascii_whitespace) {
        *offset += 1;
    }
    let start = *offset;
    while line
        .get(*offset)
        .is_some_and(|byte| !byte.is_ascii_whitespace())
    {
        *offset += 1;
    }
    (start != *offset).then(|| &line[start..*offset])
}

fn socket_inode(target: &Path) -> Option<u64> {
    let target = target.as_os_str().as_encoded_bytes();
    let inode = target.strip_prefix(b"socket:[")?.strip_suffix(b"]")?;
    std::str::from_utf8(inode).ok()?.parse().ok()
}

fn terminate_passt(pid: u32, paths: &DirectVmPaths) -> Result<()> {
    let Some(pidfd) = open_pidfd(pid)? else {
        return Ok(());
    };
    let socket = unix_socket_state(&paths.network_socket)?;
    if !socket.active()
        || !passt_process_matches(pid, &paths.network_socket)?
        || process_owns_unix_socket(pid, &socket)? == Some(false)
    {
        if wait_pidfd(&pidfd, Duration::ZERO)? {
            return Ok(());
        }
        bail!("refusing to signal PID {pid}; it no longer belongs to this direct VM");
    }
    send_pidfd_signal(&pidfd, nix::libc::SIGTERM)?;
    if wait_pidfd(&pidfd, FORCE_STOP_TIMEOUT)? {
        return Ok(());
    }
    send_pidfd_signal(&pidfd, nix::libc::SIGKILL)?;
    if wait_pidfd(&pidfd, FORCE_STOP_TIMEOUT)? {
        return Ok(());
    }
    bail!("passt PID {pid} did not exit after SIGKILL")
}

fn open_pidfd(pid: u32) -> Result<Option<File>> {
    // A pidfd pins the process identity across the validation and signal operations.
    let descriptor = unsafe { nix::libc::syscall(nix::libc::SYS_pidfd_open, pid, 0) };
    if descriptor < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(nix::libc::ESRCH) {
            return Ok(None);
        }
        return Err(error).with_context(|| format!("failed to open pidfd for passt PID {pid}"));
    }
    Ok(Some(unsafe { File::from_raw_fd(descriptor as i32) }))
}

fn send_pidfd_signal(pidfd: &File, signal: i32) -> Result<()> {
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<nix::libc::siginfo_t>(),
            0,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(nix::libc::ESRCH) {
            return Err(error).context("failed to signal passt through pidfd");
        }
    }
    Ok(())
}

fn wait_pidfd(pidfd: &File, timeout: Duration) -> Result<bool> {
    let mut pollfd = nix::libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: nix::libc::POLLIN,
        revents: 0,
    };
    let timeout = timeout.as_millis().min(i32::MAX as u128) as i32;
    loop {
        let result = unsafe { nix::libc::poll(&mut pollfd, 1, timeout) };
        if result >= 0 {
            return Ok(result > 0);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error).context("failed to wait for passt pidfd");
        }
    }
}

fn remove_stale_runtime(paths: &DirectVmPaths) -> Result<()> {
    remove_inactive_socket(&paths.socket)?;
    remove_inactive_socket(&paths.network_socket)?;
    remove_file_if_exists(&paths.pid)?;
    remove_file_if_exists(&paths.network_pid)?;
    Ok(())
}

fn remove_inactive_socket(path: &Path) -> Result<()> {
    if unix_socket_state(path)?.active() {
        bail!("refusing to remove active Unix socket {}", path.display());
    }
    if socket_ready(path)? {
        remove_file_if_exists(path)?;
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn wait_for_network_shutdown(paths: &DirectVmPaths, manifest: &DirectVmManifest) -> Result<()> {
    if !manifest
        .network
        .as_ref()
        .is_some_and(DirectVmNetwork::is_user)
    {
        return Ok(());
    }
    let deadline = Instant::now() + FORCE_STOP_TIMEOUT;
    while matches!(network_process_status(paths)?, ProcessStatus::Running(_))
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(50));
    }
    match network_process_status(paths)? {
        ProcessStatus::Stopped => Ok(()),
        ProcessStatus::Running(pid) => terminate_passt(pid, paths),
        ProcessStatus::Stale(pid) => {
            bail!("passt PID {pid} was replaced by another process during shutdown")
        }
        ProcessStatus::Untracked => {
            bail!("passt socket remains without tracked process metadata after shutdown")
        }
    }
}

fn open_append_regular(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok(file)
}

fn socket_ready(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => Ok(true),
        Ok(_) => bail!("runtime path {} is not a Unix socket", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn vm_is_running(socket: &Path) -> Result<bool> {
    let body = api_request(socket, "GET", "vm.info")?;
    let response: serde_json::Value =
        serde_json::from_str(&body).context("failed to parse Cloud Hypervisor VM info")?;
    Ok(response
        .get("state")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|state| state.eq_ignore_ascii_case("running")))
}

fn api_request(socket: &Path, method: &str, endpoint: &str) -> Result<String> {
    let mut stream = UnixStream::connect(socket).with_context(|| {
        format!(
            "failed to connect to Cloud Hypervisor API {}",
            socket.display()
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    write!(
        stream,
        "{method} /api/v1/{endpoint} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let read = stream
            .read(&mut buffer)
            .context("failed to read Cloud Hypervisor API response")?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
        if response.len() > 16 * 1024 * 1024 {
            bail!("Cloud Hypervisor API response is too large");
        }
        if expected_http_response_len(&response)?.is_some_and(|length| response.len() >= length) {
            break;
        }
    }
    let response = String::from_utf8(response)
        .context("Cloud Hypervisor returned a non-UTF-8 HTTP response")?;
    let status = response.lines().next().unwrap_or("invalid response");
    if !status.starts_with("HTTP/1.1 2") && !status.starts_with("HTTP/1.0 2") {
        bail!("Cloud Hypervisor API request {endpoint} failed: {status}");
    }
    let (_, body) = response
        .split_once("\r\n\r\n")
        .context("Cloud Hypervisor returned an invalid HTTP response")?;
    Ok(body.to_string())
}

fn expected_http_response_len(response: &[u8]) -> Result<Option<usize>> {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Ok(None);
    };
    let headers = std::str::from_utf8(&response[..header_end])
        .context("Cloud Hypervisor returned non-UTF-8 HTTP headers")?;
    let content_length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim())
    });
    let Some(content_length) = content_length else {
        return Ok(None);
    };
    let content_length = content_length
        .parse::<usize>()
        .context("Cloud Hypervisor returned an invalid Content-Length header")?;
    Ok(Some(header_end + 4 + content_length))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("qtr-direct-vm-{}", Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, b"test").unwrap();
            path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn offline_manifest() -> DirectVmManifest {
        DirectVmManifest {
            schema_version: DIRECT_VM_SCHEMA_VERSION,
            name: "edge-worker".to_string(),
            firmware: PathBuf::from("/firmware"),
            cpus: 2,
            memory_mib: 1024,
            disks: vec![DirectVmDisk {
                path: PathBuf::from("/disk.qcow2"),
                readonly: false,
            }],
            network: None,
        }
    }

    #[test]
    fn normalizes_and_serializes_direct_vm_manifest() {
        let directory = TestDirectory::new();
        let firmware = directory.file("firmware");
        let disk = directory.file("disk.raw");
        let mut manifest = DirectVmManifest {
            schema_version: DIRECT_VM_SCHEMA_VERSION,
            name: "edge-worker".to_string(),
            firmware: PathBuf::from("firmware"),
            cpus: 2,
            memory_mib: 1024,
            disks: vec![DirectVmDisk {
                path: PathBuf::from("disk.raw"),
                readonly: false,
            }],
            network: None,
        };

        normalize_manifest(&mut manifest, &directory.0).unwrap();
        assert_eq!(manifest.firmware, firmware.canonicalize().unwrap());
        assert_eq!(manifest.disks[0].path, disk.canonicalize().unwrap());
        let yaml = serde_yaml::to_string(&manifest).unwrap();
        assert!(yaml.starts_with("schemaVersion: 2\n"));
        assert!(yaml.contains("memoryMiB: 1024"));
    }

    #[test]
    fn builds_cloud_hypervisor_command_line() {
        let root = Path::new("/state");
        let paths = DirectVmPaths::new(root, "edge-worker").unwrap();
        let mut manifest = offline_manifest();
        manifest.disks[0].readonly = true;
        manifest.network = Some(DirectVmNetwork::Tap {
            tap: "tap-edge".to_string(),
            mac: "02:00:00:00:00:01".to_string(),
        });
        let args = cloud_hypervisor_args(&manifest, &paths)
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args.windows(2).any(|args| args == ["--cpus", "boot=2"]));
        assert!(
            args.windows(2)
                .any(|args| args == ["--memory", "size=1024M"])
        );
        assert!(args.iter().any(|arg| arg == "path=/disk.qcow2,readonly=on"));
        assert!(
            args.iter()
                .any(|arg| arg == "tap=tap-edge,mac=02:00:00:00:00:01")
        );
        assert!(
            args.iter()
                .any(|arg| arg == "file=/state/edge-worker/serial.log")
        );
    }

    #[test]
    fn reads_schema_one_tap_network_and_emits_schema_two() {
        let manifest = parse_manifest(
            "schemaVersion: 1\nname: edge-worker\nfirmware: /firmware\ncpus: 2\nmemoryMiB: 1024\ndisks:\n- path: /disk.raw\nnetwork:\n  tap: tap-edge\n  mac: 01:00:00:00:00:01\n",
        )
        .unwrap();

        assert_eq!(manifest.schema_version, DIRECT_VM_SCHEMA_VERSION);
        assert!(matches!(
            manifest.network,
            Some(DirectVmNetwork::Tap { ref tap, .. }) if tap == "tap-edge"
        ));
        assert!(
            serde_yaml::to_string(&manifest)
                .unwrap()
                .starts_with("schemaVersion: 2\n")
        );
    }

    #[test]
    fn defaults_user_network_mac_and_forward_address() {
        let manifest = parse_manifest(
            "schemaVersion: 2\nname: edge-worker\nfirmware: /firmware\ncpus: 2\nmemoryMiB: 1024\ndisks:\n- path: /disk.raw\nnetwork:\n  type: user\n  forwards:\n  - protocol: tcp\n    hostPort: 2222\n    guestPort: 22\n",
        )
        .unwrap();
        let Some(DirectVmNetwork::User { mac, forwards }) = manifest.network else {
            panic!("expected user network");
        };

        assert!(valid_unicast_mac(&mac));
        assert_eq!(forwards[0].host_address, default_forward_address());
    }

    #[test]
    fn builds_passt_and_vhost_user_arguments() {
        let paths = DirectVmPaths::new(Path::new("/state"), "edge-worker").unwrap();
        let mut manifest = offline_manifest();
        manifest.network = Some(DirectVmNetwork::User {
            mac: "02:00:00:00:00:01".to_string(),
            forwards: vec![DirectVmPortForward {
                protocol: DirectVmPortProtocol::Tcp,
                host_address: "127.0.0.1".parse().unwrap(),
                host_port: 2222,
                guest_port: 22,
            }],
        });

        let vmm_args = cloud_hypervisor_args(&manifest, &paths)
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            vmm_args
                .windows(2)
                .any(|args| { args == ["--memory", "size=1024M,shared=on"] })
        );
        assert!(vmm_args.iter().any(|arg| {
            arg == "vhost_user=on,socket=/state/edge-worker/network.sock,vhost_mode=client,mac=02:00:00:00:00:01"
        }));

        let passt_args = passt_args(&manifest, &paths, &["192.0.2.53".parse().unwrap()])
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            passt_args
                .windows(2)
                .any(|args| { args == ["--socket", "/state/edge-worker/network.sock"] })
        );
        assert!(
            passt_args
                .windows(2)
                .any(|args| args == ["--tcp-ports", "127.0.0.1/2222:22"])
        );
        assert!(
            passt_args
                .windows(2)
                .any(|args| args == ["--udp-ports", "none"])
        );
        assert!(
            passt_args
                .windows(2)
                .any(|args| args == ["--dns", "192.0.2.53"])
        );
    }

    #[test]
    fn rejects_duplicate_forward_bindings() {
        let mut manifest = offline_manifest();
        let forward = DirectVmPortForward {
            protocol: DirectVmPortProtocol::Tcp,
            host_address: "127.0.0.1".parse().unwrap(),
            host_port: 2222,
            guest_port: 22,
        };
        manifest.network = Some(DirectVmNetwork::User {
            mac: "02:00:00:00:00:01".to_string(),
            forwards: vec![forward.clone(), forward],
        });

        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn rejects_ipv4_mapped_forward_addresses() {
        let mut manifest = offline_manifest();
        manifest.network = Some(DirectVmNetwork::User {
            mac: "02:00:00:00:00:01".to_string(),
            forwards: vec![DirectVmPortForward {
                protocol: DirectVmPortProtocol::Tcp,
                host_address: "::ffff:127.0.0.1".parse().unwrap(),
                host_port: 2222,
                guest_port: 22,
            }],
        });

        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn parses_cloud_hypervisor_major_version() {
        assert_eq!(
            parse_cloud_hypervisor_major("cloud-hypervisor v53.0\n").unwrap(),
            53
        );
        assert!(parse_cloud_hypervisor_major("cloud-hypervisor unknown").is_err());
    }

    #[test]
    fn parses_passt_capabilities_from_either_output_stream() {
        assert_eq!(
            parse_passt_capabilities(br#"{"type":"net"}"#, b"").unwrap()["type"],
            "net"
        );
        assert_eq!(
            parse_passt_capabilities(b"", br#"{"type":"net"}"#).unwrap()["type"],
            "net"
        );
    }

    #[test]
    fn reads_non_loopback_dns_servers() {
        let directory = TestDirectory::new();
        let resolv_conf = directory.0.join("resolv.conf");
        fs::write(
            &resolv_conf,
            "nameserver 127.0.0.53\nnameserver 192.0.2.53\nnameserver 192.0.2.53\nnameserver 2001:db8::53\n",
        )
        .unwrap();

        assert_eq!(
            read_dns_servers(&resolv_conf).unwrap(),
            vec![
                "192.0.2.53".parse::<IpAddr>().unwrap(),
                "2001:db8::53".parse::<IpAddr>().unwrap()
            ]
        );
    }

    #[test]
    fn rejects_unsafe_names_and_invalid_networks() {
        assert!(validate_name("../escape").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("edge-worker").is_ok());
        assert!(valid_mac("02:00:00:00:00:01"));
        assert!(valid_mac("01:00:00:00:00:01"));
        assert!(!valid_unicast_mac("01:00:00:00:00:01"));
        assert!(!valid_mac("not-a-mac"));
    }

    #[test]
    fn defines_and_removes_stopped_direct_vm() {
        let directory = TestDirectory::new();
        let firmware = directory.file("firmware");
        let disk = directory.file("disk.raw");
        let input = directory.0.join("input.yaml");
        fs::write(
            &input,
            format!(
                "schemaVersion: 1\nname: edge-worker\nfirmware: {}\ncpus: 2\nmemoryMiB: 1024\ndisks:\n- path: {}\n",
                firmware.display(),
                disk.display()
            ),
        )
        .unwrap();

        let root = state_root(&directory.0).unwrap();
        define(&root, DirectVmDefineArgs { file: input }).unwrap();
        let paths = existing_paths(&root, "edge-worker").unwrap();
        let manifest = read_manifest(&paths.manifest).unwrap();
        assert_eq!(manifest.schema_version, DIRECT_VM_SCHEMA_VERSION);
        assert_eq!(
            runtime_status(&paths, &manifest).unwrap(),
            RuntimeStatus::Stopped
        );
        assert_eq!(
            fs::metadata(&paths.directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&paths.manifest).unwrap().permissions().mode() & 0o777,
            0o600
        );
        remove(
            &root,
            DirectVmNameArgs {
                name: "edge-worker".to_string(),
            },
        )
        .unwrap();
        assert!(!paths.directory.exists());
    }

    #[test]
    fn does_not_claim_an_unrelated_live_pid() {
        let directory = TestDirectory::new();
        let paths = DirectVmPaths::new(&directory.0, "edge-worker").unwrap();
        fs::create_dir(&paths.directory).unwrap();
        fs::write(&paths.pid, format!("{}\n", std::process::id())).unwrap();
        assert_eq!(
            runtime_status(&paths, &offline_manifest()).unwrap(),
            RuntimeStatus::Stale(std::process::id())
        );
    }

    #[test]
    fn reports_an_active_socket_without_a_pid_as_untracked() {
        use std::os::unix::net::UnixListener;

        let directory = TestDirectory::new();
        let paths = DirectVmPaths::new(&directory.0, "edge-worker").unwrap();
        fs::create_dir(&paths.directory).unwrap();
        let _listener = UnixListener::bind(&paths.socket).unwrap();

        assert_eq!(
            runtime_status(&paths, &offline_manifest()).unwrap(),
            RuntimeStatus::Untracked
        );
    }

    #[test]
    fn reports_an_unlinked_active_socket_as_untracked() {
        use std::os::unix::net::UnixListener;

        let directory = TestDirectory::new();
        let paths = DirectVmPaths::new(&directory.0, "edge-worker").unwrap();
        fs::create_dir(&paths.directory).unwrap();
        let listener = UnixListener::bind(&paths.socket).unwrap();
        fs::remove_file(&paths.socket).unwrap();

        assert_eq!(
            runtime_status(&paths, &offline_manifest()).unwrap(),
            RuntimeStatus::Untracked
        );
        assert!(remove_stale_runtime(&paths).is_err());

        drop(listener);
        assert_eq!(
            runtime_status(&paths, &offline_manifest()).unwrap(),
            RuntimeStatus::Stopped
        );
    }

    #[test]
    fn reports_an_active_socket_with_a_dead_pid_as_untracked() {
        use std::os::unix::net::UnixListener;

        let directory = TestDirectory::new();
        let paths = DirectVmPaths::new(&directory.0, "edge-worker").unwrap();
        fs::create_dir(&paths.directory).unwrap();
        fs::write(&paths.pid, format!("{}\n", u32::MAX)).unwrap();
        let _listener = UnixListener::bind(&paths.socket).unwrap();

        assert_eq!(
            runtime_status(&paths, &offline_manifest()).unwrap(),
            RuntimeStatus::Untracked
        );
    }

    #[test]
    fn cleans_an_inactive_socket_file() {
        use std::os::unix::net::UnixListener;

        let directory = TestDirectory::new();
        let paths = DirectVmPaths::new(&directory.0, "edge-worker").unwrap();
        fs::create_dir(&paths.directory).unwrap();
        drop(UnixListener::bind(&paths.socket).unwrap());

        assert_eq!(
            runtime_status(&paths, &offline_manifest()).unwrap(),
            RuntimeStatus::Stopped
        );
        remove_stale_runtime(&paths).unwrap();
        assert!(!paths.socket.exists());
    }

    #[test]
    fn reports_a_passt_socket_without_a_pid_as_untracked_network() {
        use std::os::unix::net::UnixListener;

        let directory = TestDirectory::new();
        let paths = DirectVmPaths::new(&directory.0, "edge-worker").unwrap();
        fs::create_dir(&paths.directory).unwrap();
        let _listener = UnixListener::bind(&paths.network_socket).unwrap();
        let mut manifest = offline_manifest();
        manifest.network = Some(DirectVmNetwork::User {
            mac: "02:00:00:00:00:01".to_string(),
            forwards: Vec::new(),
        });

        assert_eq!(
            runtime_status(&paths, &manifest).unwrap(),
            RuntimeStatus::UntrackedNetwork
        );
    }

    #[test]
    fn passt_identity_requires_a_socket_option_pair() {
        let socket = Path::new("/state/edge-worker/network.sock");
        assert!(cmdline_matches_passt(
            b"passt\0--foreground\0--vhost-user\0--socket\0/state/edge-worker/network.sock\0",
            socket
        ));
        assert!(!cmdline_matches_passt(
            b"other\0--vhost-user\0--log-file\0/state/edge-worker/network.sock\0",
            socket
        ));
        assert!(!cmdline_matches_passt(
            b"passt\0--socket\0/state/edge-worker/network.sock\0",
            socket
        ));
    }

    #[test]
    fn recovers_and_terminates_passt_without_a_pid_file() {
        use std::os::unix::net::UnixListener;

        let directory = TestDirectory::new();
        let paths = DirectVmPaths::new(&directory.0, "edge-worker").unwrap();
        fs::create_dir(&paths.directory).unwrap();
        let listener = UnixListener::bind(&paths.network_socket).unwrap();
        let flags = unsafe { nix::libc::fcntl(listener.as_raw_fd(), nix::libc::F_GETFD) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe {
                nix::libc::fcntl(
                    listener.as_raw_fd(),
                    nix::libc::F_SETFD,
                    flags & !nix::libc::FD_CLOEXEC,
                )
            },
            0
        );
        let (input, _input_peer) = UnixStream::pair().unwrap();
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("read value")
            .arg("--vhost-user")
            .arg("--socket")
            .arg(&paths.network_socket)
            .stdin(Stdio::from(std::os::fd::OwnedFd::from(input)))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        drop(listener);

        let deadline = Instant::now() + Duration::from_secs(2);
        let recovered = loop {
            if network_process_status(&paths).unwrap() == ProcessStatus::Running(child.id()) {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(10));
        };
        if !recovered {
            let _ = child.kill();
            let _ = child.wait();
            panic!("failed to recover inherited passt socket owner");
        }

        terminate_passt(child.id(), &paths).unwrap();
        child.wait().unwrap();
        assert_eq!(
            network_process_status(&paths).unwrap(),
            ProcessStatus::Stopped
        );
    }

    #[test]
    fn reads_api_response_without_waiting_for_connection_close() {
        use std::{os::unix::net::UnixListener, sync::mpsc};

        let directory = TestDirectory::new();
        let socket = directory.0.join("api.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 256];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nConnection: keep-alive\r\nContent-Length: 19\r\n\r\n{\"state\":\"Running\"}",
                )
                .unwrap();
            release_rx.recv().unwrap();
        });

        assert_eq!(
            api_request(&socket, "GET", "vm.info").unwrap(),
            "{\"state\":\"Running\"}"
        );
        release_tx.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn rejects_an_existing_state_root_with_broad_permissions() {
        let directory = TestDirectory::new();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(state_root(&directory.0).is_err());
    }
}
