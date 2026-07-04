use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use duct::cmd;
use serde::{Deserialize, Serialize};

use crate::config::{
    StorageAddCommand, StorageAddIscsiArgs, StorageArgs, StorageBackendArgs, StorageCommand,
    StorageVolumeArgs,
};

const ISCSIADM: &str = "iscsiadm";
const ISCSID_SERVICE: &str = "iscsid";
const DEV_DISK_BY_PATH: &str = "/dev/disk/by-path";

pub fn run(args: StorageArgs) -> Result<()> {
    let config = args.config;

    match args.command {
        StorageCommand::Status => status(),
        StorageCommand::Add(args) => add(&config, args.command),
        StorageCommand::List => list(&config),
        StorageCommand::Scan(args) => scan(&config, args),
        StorageCommand::Volumes(args) => volumes(&config, args),
        StorageCommand::Connect(args) => connect(&config, args),
        StorageCommand::Disconnect(args) => disconnect(&config, args),
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StorageState {
    #[serde(default)]
    backends: Vec<StorageBackend>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageBackend {
    name: String,
    #[serde(flatten)]
    driver: StorageDriver,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "driver", rename_all = "lowercase")]
enum StorageDriver {
    Iscsi {
        address: String,
        port: u16,
        #[serde(default)]
        volumes: Vec<IscsiVolume>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IscsiVolume {
    name: String,
    target: String,
    portal: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IscsiNode {
    portal: String,
    target: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IscsiSession {
    target: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IscsiDevice {
    path: PathBuf,
    target: String,
    lun: u32,
}

fn status() -> Result<()> {
    let iscsiadm = which::which(ISCSIADM).is_ok();
    let iscsid = matches!(
        cmd!("systemctl", "is-active", "--quiet", ISCSID_SERVICE)
            .stdout_null()
            .stderr_null()
            .unchecked()
            .run(),
        Ok(output) if output.status.success()
    );

    println!(
        "iscsiadm: {}",
        if iscsiadm { "available" } else { "missing" }
    );
    println!("iscsid: {}", if iscsid { "active" } else { "inactive" });

    Ok(())
}

fn add(config_path: &Path, command: StorageAddCommand) -> Result<()> {
    match command {
        StorageAddCommand::Iscsi(args) => add_iscsi(config_path, args),
    }
}

fn add_iscsi(config_path: &Path, args: StorageAddIscsiArgs) -> Result<()> {
    validate_name("backend", &args.name)?;

    let mut state = load_state(config_path)?;
    if state
        .backends
        .iter()
        .any(|backend| backend.name == args.name)
    {
        bail!("storage backend {} already exists", args.name);
    }

    state.backends.push(StorageBackend {
        name: args.name.clone(),
        driver: StorageDriver::Iscsi {
            address: args.address,
            port: args.port,
            volumes: Vec::new(),
        },
    });
    save_state(config_path, &state)?;

    eprintln!("[qtr] added storage backend: {}", args.name);
    Ok(())
}

fn list(config_path: &Path) -> Result<()> {
    let state = load_state(config_path)?;

    println!("{:<24} DRIVER", "NAME");
    for backend in state.backends {
        println!("{:<24} {}", backend.name, backend.driver.name());
    }

    Ok(())
}

fn scan(config_path: &Path, args: StorageBackendArgs) -> Result<()> {
    let mut state = load_state(config_path)?;
    let backend = find_backend_mut(&mut state, &args.name)?;

    match &mut backend.driver {
        StorageDriver::Iscsi {
            address,
            port,
            volumes,
        } => {
            let nodes = IscsiAdm::new()
                .discover(address, *port)
                .with_context(|| format!("failed to scan storage backend {}", args.name))?;
            merge_iscsi_nodes(volumes, nodes)?;
            save_state(config_path, &state)?;
            eprintln!("[qtr] scanned storage backend: {}", args.name);
        }
    }

    volumes(config_path, args)
}

fn volumes(config_path: &Path, args: StorageBackendArgs) -> Result<()> {
    let state = load_state(config_path)?;
    let backend = find_backend(&state, &args.name)?;

    match &backend.driver {
        StorageDriver::Iscsi { volumes, .. } => {
            let sessions = IscsiAdm::new().sessions().unwrap_or_default();
            let devices = iscsi_devices_from_by_path(Path::new(DEV_DISK_BY_PATH))?;
            print_iscsi_volumes(volumes, &sessions, &devices);
        }
    }

    Ok(())
}

fn connect(config_path: &Path, args: StorageVolumeArgs) -> Result<()> {
    let state = load_state(config_path)?;
    let (backend_name, volume_name) = parse_volume_ref(&args.volume)?;
    let volume = find_iscsi_volume(&state, backend_name, volume_name)?;

    IscsiAdm::new()
        .login(volume)
        .with_context(|| format!("failed to connect storage volume {}", args.volume))?;

    eprintln!("[qtr] connected storage volume: {}", args.volume);
    if let Some(device) = find_device_for_target(&volume.target)? {
        eprintln!("[qtr] device: {}", device.path.display());
    }

    Ok(())
}

fn disconnect(config_path: &Path, args: StorageVolumeArgs) -> Result<()> {
    let state = load_state(config_path)?;
    let (backend_name, volume_name) = parse_volume_ref(&args.volume)?;
    let volume = find_iscsi_volume(&state, backend_name, volume_name)?;

    IscsiAdm::new()
        .logout(volume)
        .with_context(|| format!("failed to disconnect storage volume {}", args.volume))?;

    eprintln!("[qtr] disconnected storage volume: {}", args.volume);
    Ok(())
}

impl StorageDriver {
    fn name(&self) -> &'static str {
        match self {
            Self::Iscsi { .. } => "iscsi",
        }
    }
}

struct IscsiAdm;

impl IscsiAdm {
    fn new() -> Self {
        Self
    }

    fn discover(&self, address: &str, port: u16) -> Result<Vec<IscsiNode>> {
        let endpoint = format!("{address}:{port}");
        let output = duct::cmd(
            ISCSIADM,
            [
                "-m".to_string(),
                "discovery".to_string(),
                "-t".to_string(),
                "sendtargets".to_string(),
                "-p".to_string(),
                endpoint,
            ],
        )
        .read()
        .context("failed to run iscsiadm")?;

        Ok(parse_discovery_nodes(&output))
    }

    fn sessions(&self) -> Result<Vec<IscsiSession>> {
        let output = duct::cmd(ISCSIADM, ["-m", "session"])
            .stdout_capture()
            .stderr_capture()
            .unchecked()
            .run()
            .context("failed to run iscsiadm")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("No active sessions") || stderr.contains("No matching sessions") {
                return Ok(Vec::new());
            }
            bail!("iscsiadm failed to list active sessions");
        }

        let stdout = String::from_utf8(output.stdout).context("iscsiadm output was not UTF-8")?;
        Ok(parse_sessions(&stdout))
    }

    fn login(&self, volume: &IscsiVolume) -> Result<()> {
        duct::cmd(
            ISCSIADM,
            [
                "-m",
                "node",
                "-T",
                volume.target.as_str(),
                "-p",
                volume.portal.as_str(),
                "--login",
            ],
        )
        .run()
        .context("failed to run iscsiadm")?;

        Ok(())
    }

    fn logout(&self, volume: &IscsiVolume) -> Result<()> {
        duct::cmd(
            ISCSIADM,
            [
                "-m",
                "node",
                "-T",
                volume.target.as_str(),
                "-p",
                volume.portal.as_str(),
                "--logout",
            ],
        )
        .run()
        .context("failed to run iscsiadm")?;

        Ok(())
    }
}

fn load_state(path: &Path) -> Result<StorageState> {
    if !path.exists() {
        return Ok(StorageState::default());
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read storage config {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(StorageState::default());
    }

    serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse storage config {}", path.display()))
}

fn save_state(path: &Path, state: &StorageState) -> Result<()> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let content = serde_yaml::to_string(state).context("failed to serialize storage config")?;
    fs::write(path, content)
        .with_context(|| format!("failed to write storage config {}", path.display()))
}

fn find_backend<'a>(state: &'a StorageState, name: &str) -> Result<&'a StorageBackend> {
    state
        .backends
        .iter()
        .find(|backend| backend.name == name)
        .with_context(|| format!("failed to find storage backend {name}"))
}

fn find_backend_mut<'a>(state: &'a mut StorageState, name: &str) -> Result<&'a mut StorageBackend> {
    state
        .backends
        .iter_mut()
        .find(|backend| backend.name == name)
        .with_context(|| format!("failed to find storage backend {name}"))
}

fn find_iscsi_volume<'a>(
    state: &'a StorageState,
    backend_name: &str,
    volume_name: &str,
) -> Result<&'a IscsiVolume> {
    let backend = find_backend(state, backend_name)?;
    match &backend.driver {
        StorageDriver::Iscsi { volumes, .. } => volumes
            .iter()
            .find(|volume| volume.name == volume_name)
            .with_context(|| format!("failed to find storage volume {backend_name}/{volume_name}")),
    }
}

fn merge_iscsi_nodes(volumes: &mut Vec<IscsiVolume>, nodes: Vec<IscsiNode>) -> Result<()> {
    for node in nodes {
        if volumes.iter().any(|volume| volume.target == node.target) {
            continue;
        }

        let name = unique_volume_name(volumes, &volume_name_from_target(&node.target));
        validate_name("volume", &name)?;
        volumes.push(IscsiVolume {
            name,
            target: node.target,
            portal: node.portal,
        });
    }

    Ok(())
}

fn print_iscsi_volumes(
    volumes: &[IscsiVolume],
    sessions: &[IscsiSession],
    devices: &[IscsiDevice],
) {
    println!("{:<24} {:<12} DEVICE", "VOLUME", "STATE");
    for volume in volumes {
        let connected = sessions
            .iter()
            .any(|session| session.target == volume.target);
        let device = devices
            .iter()
            .find(|device| device.target == volume.target)
            .map(|device| device.path.display().to_string())
            .unwrap_or_else(|| "-".to_string());
        let state = if connected { "connected" } else { "available" };

        println!("{:<24} {:<12} {}", volume.name, state, device);
    }
}

fn find_device_for_target(target: &str) -> Result<Option<IscsiDevice>> {
    Ok(iscsi_devices_from_by_path(Path::new(DEV_DISK_BY_PATH))?
        .into_iter()
        .find(|device| device.target == target))
}

fn iscsi_devices_from_by_path(path: &Path) -> Result<Vec<IscsiDevice>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut devices = Vec::new();
    for entry in fs::read_dir(path)
        .with_context(|| format!("failed to read device directory {}", path.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read entry in {}", path.display()))?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some((target, lun)) = parse_by_path_iscsi_device(file_name) else {
            continue;
        };

        devices.push(IscsiDevice {
            path: entry.path(),
            target,
            lun,
        });
    }

    devices.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(devices)
}

fn parse_discovery_nodes(output: &str) -> Vec<IscsiNode> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let portal = fields.next()?;
            let target = fields.next()?;
            Some(IscsiNode {
                portal: portal.to_string(),
                target: target.to_string(),
            })
        })
        .collect()
}

fn parse_sessions(output: &str) -> Vec<IscsiSession> {
    output
        .lines()
        .filter_map(|line| {
            let target = line
                .split_whitespace()
                .find(|field| field.starts_with("iqn.") || field.starts_with("eui."))?;
            Some(IscsiSession {
                target: target.to_string(),
            })
        })
        .collect()
}

fn parse_by_path_iscsi_device(file_name: &str) -> Option<(String, u32)> {
    let (_, rest) = file_name.split_once("-iscsi-")?;
    let (target, lun) = rest.rsplit_once("-lun-")?;
    let lun = lun.parse::<u32>().ok()?;

    Some((target.to_string(), lun))
}

fn parse_volume_ref(value: &str) -> Result<(&str, &str)> {
    let Some((backend, volume)) = value.split_once('/') else {
        bail!("volume must use backend/volume format");
    };
    if backend.is_empty() || volume.is_empty() || volume.contains('/') {
        bail!("volume must use backend/volume format");
    }

    Ok((backend, volume))
}

fn volume_name_from_target(target: &str) -> String {
    let raw = target
        .rsplit_once(':')
        .map(|(_, name)| name)
        .unwrap_or(target);
    let name = sanitize_name(raw);

    if name.is_empty() {
        "volume".to_string()
    } else {
        name
    }
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .filter_map(|ch| match ch {
            ch if ch.is_ascii_alphanumeric() => Some(ch.to_ascii_lowercase()),
            '-' | '_' | '.' => Some(ch),
            ':' => Some('-'),
            _ => None,
        })
        .collect()
}

fn unique_volume_name(volumes: &[IscsiVolume], base: &str) -> String {
    if !volumes.iter().any(|volume| volume.name == base) {
        return base.to_string();
    }

    for index in 2.. {
        let candidate = format!("{base}-{index}");
        if !volumes.iter().any(|volume| volume.name == candidate) {
            return candidate;
        }
    }

    unreachable!()
}

fn validate_name(kind: &str, name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("{kind} name must not be empty");
    }

    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("{kind} name may only contain ASCII letters, digits, '-', '_' and '.'");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_discovery_nodes() {
        let output = "10.0.0.10:3260,1 iqn.2026-01.local:qtr.db\n\
                      10.0.0.11:3260,1 iqn.2026-01.local:qtr.logs\n";

        assert_eq!(
            parse_discovery_nodes(output),
            vec![
                IscsiNode {
                    portal: "10.0.0.10:3260,1".to_string(),
                    target: "iqn.2026-01.local:qtr.db".to_string(),
                },
                IscsiNode {
                    portal: "10.0.0.11:3260,1".to_string(),
                    target: "iqn.2026-01.local:qtr.logs".to_string(),
                },
            ]
        );
    }

    #[test]
    fn parses_active_sessions() {
        let output = "tcp: [1] 10.0.0.10:3260,1 iqn.2026-01.local:qtr.db (non-flash)\n";

        assert_eq!(
            parse_sessions(output),
            vec![IscsiSession {
                target: "iqn.2026-01.local:qtr.db".to_string(),
            }]
        );
    }

    #[test]
    fn parses_by_path_iscsi_devices() {
        let name = "ip-10.0.0.10:3260-iscsi-iqn.2026-01.local:qtr.db-lun-0";

        assert_eq!(
            parse_by_path_iscsi_device(name),
            Some(("iqn.2026-01.local:qtr.db".to_string(), 0))
        );
    }

    #[test]
    fn derives_user_facing_volume_names_from_targets() {
        assert_eq!(
            volume_name_from_target("iqn.2026-01.local:qtr.DB_Data"),
            "qtr.db_data"
        );
        assert_eq!(
            volume_name_from_target("iqn.2026-01.local"),
            "iqn.2026-01.local"
        );
    }

    #[test]
    fn validates_volume_refs() {
        assert_eq!(parse_volume_ref("lab-san/db").unwrap(), ("lab-san", "db"));
        assert!(parse_volume_ref("db").is_err());
        assert!(parse_volume_ref("lab-san/").is_err());
        assert!(parse_volume_ref("lab-san/db/extra").is_err());
    }

    #[test]
    fn parses_storage_state_with_iscsi_backend() {
        let state: StorageState = serde_yaml::from_str(
            r#"backends:
- name: lab-san
  driver: iscsi
  address: 10.0.0.10
  port: 3260
  volumes: []
"#,
        )
        .unwrap();

        assert_eq!(state.backends.len(), 1);
        assert_eq!(state.backends[0].name, "lab-san");
        assert_eq!(state.backends[0].driver.name(), "iscsi");
    }
}
