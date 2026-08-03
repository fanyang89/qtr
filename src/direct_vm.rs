use std::{
    ffi::OsString,
    fmt,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
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

const DIRECT_VM_SCHEMA_VERSION: u64 = 1;
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectVmDisk {
    path: PathBuf,
    #[serde(default)]
    readonly: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectVmNetwork {
    tap: String,
    mac: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeStatus {
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
            Self::Stale(_) => formatter.write_str("stale"),
            Self::Untracked => formatter.write_str("untracked"),
        }
    }
}

struct DirectVmPaths {
    directory: PathBuf,
    manifest: PathBuf,
    pid: PathBuf,
    socket: PathBuf,
    serial_log: PathBuf,
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
            serial_log: directory.join("serial.log"),
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
        DirectVmCommand::Start(command) => start(&root, &args.cloud_hypervisor, command),
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
    let mut manifest: DirectVmManifest = serde_yaml::from_str(&contents)
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
        rows.push((manifest.name, runtime_status(&paths)?));
    }
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    cli_table::print_table(
        &["NAME", "STATE", "PID"],
        rows.into_iter().map(|(name, status)| {
            let pid = match status {
                RuntimeStatus::Running(pid) | RuntimeStatus::Stale(pid) => pid.to_string(),
                RuntimeStatus::Stopped | RuntimeStatus::Untracked => "-".to_string(),
            };
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

fn start(root: &Path, executable: &Path, args: DirectVmNameArgs) -> Result<()> {
    let paths = existing_paths(root, &args.name)?;
    let _lock = lock(root, &args.name)?;
    match runtime_status(&paths)? {
        RuntimeStatus::Running(pid) => {
            bail!("direct VM {} is already running as PID {pid}", args.name)
        }
        RuntimeStatus::Stale(pid) => bail!(
            "direct VM {} has PID {pid} owned by another process; remove {} after verifying the process",
            args.name,
            paths.pid.display()
        ),
        RuntimeStatus::Untracked => bail!(
            "direct VM {} has a responsive API socket without tracked process metadata",
            args.name
        ),
        RuntimeStatus::Stopped => {}
    }
    remove_stale_runtime(&paths)?;
    let manifest = read_manifest(&paths.manifest)?;
    validate_runtime_resources(&manifest)?;
    let vmm_log = open_append_regular(&paths.vmm_log)?;
    drop(open_append_regular(&paths.serial_log)?);
    let stderr = vmm_log
        .try_clone()
        .context("failed to clone VMM log handle")?;
    let mut child = Command::new(executable)
        .args(cloud_hypervisor_args(&manifest, &paths))
        .stdin(Stdio::null())
        .stdout(Stdio::from(vmm_log))
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| {
            format!(
                "failed to start Cloud Hypervisor using {}",
                executable.display()
            )
        })?;

    if let Err(error) = write_pid(&paths.pid, child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }

    let deadline = Instant::now() + START_TIMEOUT;
    let mut last_api_error = None;
    loop {
        let socket_ready = match socket_ready(&paths.socket) {
            Ok(ready) => ready,
            Err(error) => {
                stop_spawned_child(&mut child, &paths.socket);
                remove_stale_runtime(&paths)?;
                return Err(error);
            }
        };
        if socket_ready {
            match vm_is_running(&paths.socket) {
                Ok(true) => {
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
        if let Some(status) = child
            .try_wait()
            .context("failed to query Cloud Hypervisor process")?
        {
            remove_stale_runtime(&paths)?;
            bail!(
                "Cloud Hypervisor exited with {status} before creating {}; see {}",
                paths.socket.display(),
                paths.vmm_log.display()
            );
        }
        if Instant::now() >= deadline {
            stop_spawned_child(&mut child, &paths.socket);
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

fn stop_spawned_child(child: &mut Child, socket: &Path) {
    if socket_ready(socket).unwrap_or(false) {
        let _ = api_request(socket, "PUT", "vmm.shutdown");
        let deadline = Instant::now() + FORCE_STOP_TIMEOUT;
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn stop(root: &Path, args: DirectVmStopArgs) -> Result<()> {
    let paths = existing_paths(root, &args.name)?;
    let _lock = lock(root, &args.name)?;
    let status = runtime_status(&paths)?;
    let RuntimeStatus::Running(pid) = status else {
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
            RuntimeStatus::Untracked => bail!(
                "direct VM {} has a responsive API socket without tracked process metadata",
                args.name
            ),
            RuntimeStatus::Stopped | RuntimeStatus::Running(_) => unreachable!(),
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
    remove_stale_runtime(&paths)?;
    eprintln!("[qtr] stopped direct VM: {}", args.name);
    Ok(())
}

fn remove(root: &Path, args: DirectVmNameArgs) -> Result<()> {
    let paths = existing_paths(root, &args.name)?;
    let _lock = lock(root, &args.name)?;
    match runtime_status(&paths)? {
        RuntimeStatus::Running(pid) => {
            bail!(
                "direct VM {} is running as PID {pid}; stop it first",
                args.name
            )
        }
        RuntimeStatus::Stale(pid) => bail!(
            "direct VM {} has PID {pid} owned by another process; inspect {} before removal",
            args.name,
            paths.pid.display()
        ),
        RuntimeStatus::Untracked => bail!(
            "direct VM {} has a responsive API socket without tracked process metadata",
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
    let manifest = serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse direct VM manifest {}", path.display()))?;
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
        if network.tap.is_empty()
            || !network
                .tap
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            bail!("direct VM TAP name contains unsupported characters");
        }
        if !valid_mac(&network.mac) {
            bail!("direct VM network MAC address is invalid");
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
    if let Some(network) = &manifest.network {
        let tap = Path::new("/sys/class/net").join(&network.tap);
        if !tap.exists() {
            bail!("pre-created TAP interface {} does not exist", network.tap);
        }
        if !tap.join("tun_flags").exists() {
            bail!("network interface {} is not a TAP device", network.tap);
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
    let mut args = vec![
        "--api-socket".into(),
        paths.socket.as_os_str().into(),
        "--firmware".into(),
        manifest.firmware.as_os_str().into(),
        "--cpus".into(),
        format!("boot={}", manifest.cpus).into(),
        "--memory".into(),
        format!("size={}M", manifest.memory_mib).into(),
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
        args.push(format!("tap={},mac={}", network.tap, network.mac).into());
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

fn runtime_status(paths: &DirectVmPaths) -> Result<RuntimeStatus> {
    let Some(pid) = read_pid(&paths.pid)? else {
        return if socket_ready(&paths.socket)? && api_is_responsive(&paths.socket) {
            Ok(RuntimeStatus::Untracked)
        } else {
            Ok(RuntimeStatus::Stopped)
        };
    };
    if !Path::new("/proc").join(pid.to_string()).exists() {
        return Ok(RuntimeStatus::Stopped);
    }
    if process_matches(pid, &paths.socket)? {
        Ok(RuntimeStatus::Running(pid))
    } else {
        Ok(RuntimeStatus::Stale(pid))
    }
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
    let cmdline_path = Path::new("/proc").join(pid.to_string()).join("cmdline");
    let cmdline = match fs::read(&cmdline_path) {
        Ok(cmdline) => cmdline,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect process command line for PID {pid}"));
        }
    };
    let socket = socket.as_os_str().as_encoded_bytes();
    Ok(cmdline
        .split(|byte| *byte == 0)
        .any(|argument| argument == socket))
}

fn remove_stale_runtime(paths: &DirectVmPaths) -> Result<()> {
    for path in [&paths.pid, &paths.socket] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to remove {}", path.display()));
            }
        }
    }
    Ok(())
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
        Ok(_) => bail!(
            "Cloud Hypervisor API path {} is not a Unix socket",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn api_is_responsive(socket: &Path) -> bool {
    api_request(socket, "GET", "vmm.ping").is_ok()
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

    #[test]
    fn normalizes_and_serializes_direct_vm_manifest() {
        let directory = TestDirectory::new();
        let firmware = directory.file("firmware");
        let disk = directory.file("disk.raw");
        let mut manifest = DirectVmManifest {
            schema_version: 1,
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
        assert!(yaml.starts_with("schemaVersion: 1\n"));
        assert!(yaml.contains("memoryMiB: 1024"));
    }

    #[test]
    fn builds_cloud_hypervisor_command_line() {
        let root = Path::new("/state");
        let paths = DirectVmPaths::new(root, "edge-worker").unwrap();
        let manifest = DirectVmManifest {
            schema_version: 1,
            name: "edge-worker".to_string(),
            firmware: PathBuf::from("/firmware"),
            cpus: 2,
            memory_mib: 1024,
            disks: vec![DirectVmDisk {
                path: PathBuf::from("/disk.qcow2"),
                readonly: true,
            }],
            network: Some(DirectVmNetwork {
                tap: "tap-edge".to_string(),
                mac: "02:00:00:00:00:01".to_string(),
            }),
        };
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
    fn rejects_unsafe_names_and_invalid_networks() {
        assert!(validate_name("../escape").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("edge-worker").is_ok());
        assert!(valid_mac("02:00:00:00:00:01"));
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
        assert_eq!(runtime_status(&paths).unwrap(), RuntimeStatus::Stopped);
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
            runtime_status(&paths).unwrap(),
            RuntimeStatus::Stale(std::process::id())
        );
    }

    #[test]
    fn reports_a_responsive_socket_without_a_pid_as_untracked() {
        use std::os::unix::net::UnixListener;

        let directory = TestDirectory::new();
        let paths = DirectVmPaths::new(&directory.0, "edge-worker").unwrap();
        fs::create_dir(&paths.directory).unwrap();
        let listener = UnixListener::bind(&paths.socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 256];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });

        assert_eq!(runtime_status(&paths).unwrap(), RuntimeStatus::Untracked);
        server.join().unwrap();
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
