use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, IsTerminal, Write},
    net::IpAddr,
    ops::Range,
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use roxmltree::{Document, Node};
use serde::Serialize;
use similar::TextDiff;
use uuid::Uuid;
use virt::{
    connect::Connect,
    domain::{Domain, DomainInfo, MemoryStat},
    error::clear_error_callback,
    sys,
};

use crate::{
    config::{
        ColorMode, DiskFormat, GraphicsMode, VmApplyArgs, VmArgs, VmAutostartArgs,
        VmCapabilitiesArgs, VmCommand, VmCpArgs, VmDumpArgs, VmExecArgs, VmInitArgs, VmListArgs,
        VmNameArgs, VmRemoveArgs, VmSavedStateArgs, VmStartArgs, VmStopArgs,
    },
    domain_xml::{
        self, BootDevice, GraphicsSpec, VmLaunchCpuSpec, VmLaunchDiskSource, VmLaunchDiskSpec,
        VmLaunchDomainSpec, VmLaunchIoThreadsSpec, build_vm_launch_domain_xml, parse_boot_devices,
    },
    guest_agent, vm_reconcile,
};

pub use crate::vm_model::{
    VmCdrom, VmCdromEntry, VmCpu, VmCpuMode, VmCpuTopology, VmDisk, VmDiskBus, VmDiskCache,
    VmDiskDetectZeroes, VmDiskDiscard, VmDiskEntry, VmDiskIoConfig, VmDiskIoMode, VmDiskIoTune,
    VmDiskIoTuneConfig, VmDiskSerial, VmDiskType, VmIoThreads, VmMachine, VmManifest, VmMemory,
};

pub fn run(args: VmArgs) -> Result<()> {
    clear_error_callback();

    match args.command {
        VmCommand::Capabilities(args) => capabilities(args),
        VmCommand::Init(args) => init(args),
        VmCommand::Apply(args) => apply(args),
        VmCommand::Dump(args) => dump(args),
        VmCommand::List(args) => list(args),
        VmCommand::Start(args) => start(args),
        VmCommand::Stop(args) => stop(args),
        VmCommand::Reboot(args) => reboot(args),
        VmCommand::Reset(args) => reset(args),
        VmCommand::Suspend(args) => suspend(args),
        VmCommand::Resume(args) => resume(args),
        VmCommand::Autostart(args) => autostart(args),
        VmCommand::Save(args) => managed_save(args),
        VmCommand::Restore(args) => restore_managed_save(args),
        VmCommand::SavedState(args) => managed_save_state(args),
        VmCommand::Rm(args) => remove(args),
        VmCommand::Vnc(args) => vnc(args),
        VmCommand::Exec(args) => exec(args),
        VmCommand::Cp(args) => cp(args),
    }
}

const VM_MANIFEST_SCHEMA_VERSION: u64 = 2;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmCapabilities {
    pub emulator: Option<String>,
    pub domain_type: String,
    pub architecture: String,
    pub machine: Option<String>,
    pub max_vcpus: Option<u32>,
    pub firmware: Vec<String>,
    pub cpu_modes: Vec<String>,
    pub devices: Vec<VmDeviceCapability>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmDeviceCapability {
    pub device: String,
    pub options: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmSummary {
    pub name: String,
    pub state: &'static str,
    pub id: Option<String>,
    pub vnc: bool,
    pub vnc_endpoint: Option<String>,
    pub serial_log: Option<String>,
    pub memory_mib: Option<u64>,
    pub vcpus: Option<u32>,
    pub io_threads: Option<VmIoThreads>,
    pub network: Option<String>,
    pub disks: Option<Vec<VmSummaryDisk>>,
    pub cdrom: Option<String>,
    pub boot: Option<Vec<String>>,
    pub graphics: String,
    pub vnc_listen: Option<String>,
    pub vnc_port: Option<u16>,
    pub metrics: Option<VmMetrics>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmSummaryDisk {
    #[serde(rename = "type")]
    pub disk_type: VmDiskType,
    pub path: String,
    pub format: DiskFormat,
    pub target: String,
    pub bus: VmDiskBus,
    pub cache: Option<VmDiskCache>,
    pub io: Option<VmDiskIoConfig>,
}

#[derive(Debug)]
pub enum VmApiError {
    InvalidRequest(anyhow::Error),
    NotFound(String),
    Conflict(anyhow::Error),
    Internal(anyhow::Error),
}

pub type VmApiResult<T> = std::result::Result<T, VmApiError>;

impl std::fmt::Display for VmApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(error) | Self::Conflict(error) | Self::Internal(error) => {
                error.fmt(formatter)
            }
            Self::NotFound(name) => write!(formatter, "domain {name} not found"),
        }
    }
}

impl std::error::Error for VmApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRequest(error) | Self::Conflict(error) | Self::Internal(error) => {
                Some(error.as_ref())
            }
            Self::NotFound(_) => None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VmExecOutput {
    domain: String,
    mode: VmExecMode,
    command: String,
    script: Option<String>,
    guest_path: Option<String>,
    exit_code: i32,
    elapsed_ms: u128,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum VmExecMode {
    Command,
    Script,
}

#[derive(Debug)]
enum VmCopyEndpoint {
    Host(PathBuf),
    Guest(String),
}

#[derive(Debug)]
struct GuestOutputStream {
    path: String,
    offset: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmMetrics {
    pub cpu_time_ns: u64,
    pub memory_used_mib: u64,
    pub memory_total_mib: u64,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub sampled_at_ms: u64,
}

fn default_vm_vnc_listen() -> String {
    "127.0.0.1".to_string()
}

fn effective_memory(manifest: &VmManifest) -> Result<VmMemory> {
    match manifest.memory {
        Some(memory) => Ok(memory),
        None => Ok(VmMemory {
            size_mib: manifest
                .memory_gib
                .checked_mul(1024)
                .context("memoryGiB is too large")?,
            max_mib: None,
        }),
    }
}

fn effective_vcpus(manifest: &VmManifest) -> Result<u32> {
    let Some(cpu) = &manifest.cpu else {
        return Ok(manifest.vcpus);
    };

    match (cpu.vcpus, cpu.topology) {
        (Some(vcpus), None) => Ok(vcpus),
        (None, Some(topology)) => topology.vcpus(),
        (Some(_), Some(_)) => bail!("cpu.vcpus and cpu.topology are mutually exclusive"),
        (None, None) => bail!("cpu requires vcpus or topology"),
    }
}

fn launch_cpu(manifest: &VmManifest) -> Option<VmLaunchCpuSpec<'_>> {
    manifest.cpu.as_ref().map(|cpu| VmLaunchCpuSpec {
        mode: cpu.mode.as_xml(),
        model: cpu.model.as_deref(),
        topology: cpu.topology.map(VmCpuTopology::launch),
    })
}

pub fn parse_manifest_yaml(input: &str) -> Result<VmManifest> {
    let value: serde_yaml::Value =
        serde_yaml::from_str(input).context("failed to parse VM YAML document")?;
    let mut mapping = match value {
        serde_yaml::Value::Mapping(mapping) => mapping,
        _ => bail!("VM YAML document must be a mapping"),
    };
    let schema_key = serde_yaml::Value::String("schemaVersion".to_string());
    let has_key = |key: &str| mapping.contains_key(serde_yaml::Value::String(key.to_string()));

    if has_key("memory") && has_key("memoryGiB") {
        bail!("memory and memoryGiB are mutually exclusive");
    }
    if has_key("cpu") && has_key("vcpus") {
        bail!("cpu and vcpus are mutually exclusive");
    }
    if has_key("cdrom") && has_key("cdroms") {
        bail!("cdrom and cdroms are mutually exclusive");
    }
    let has_cdroms = has_key("cdroms");

    let version = match mapping.remove(&schema_key) {
        Some(version) => version
            .as_u64()
            .context("schemaVersion must be a positive integer")?,
        None => 1,
    };
    if !(1..=VM_MANIFEST_SCHEMA_VERSION).contains(&version) {
        bail!("unsupported VM schemaVersion {version}; expected 1 to {VM_MANIFEST_SCHEMA_VERSION}");
    }
    if version == 1
        && mapping
            .get(serde_yaml::Value::String("disks".to_string()))
            .and_then(serde_yaml::Value::as_sequence)
            .is_some_and(|disks| {
                disks.iter().any(|disk| {
                    disk.as_mapping().is_some_and(|disk| {
                        disk.contains_key(serde_yaml::Value::String("state".to_string()))
                    })
                })
            })
    {
        bail!("disk state requires schemaVersion 2");
    }
    if version == 1 && has_cdroms {
        bail!("cdroms requires schemaVersion 2");
    }

    serde_yaml::from_value(serde_yaml::Value::Mapping(mapping))
        .context("failed to parse VM manifest")
}

pub fn serialize_manifest_yaml(manifest: &VmManifest) -> Result<String> {
    let value = serde_yaml::to_value(manifest).context("failed to serialize VM manifest")?;
    let serde_yaml::Value::Mapping(mut manifest_mapping) = value else {
        unreachable!("VmManifest must serialize to a mapping");
    };
    if manifest.memory.is_some() {
        manifest_mapping.remove(serde_yaml::Value::String("memoryGiB".to_string()));
    }
    if manifest.cpu.is_some() {
        manifest_mapping.remove(serde_yaml::Value::String("vcpus".to_string()));
    }
    let mut document = serde_yaml::Mapping::new();
    document.insert(
        serde_yaml::Value::String("schemaVersion".to_string()),
        serde_yaml::Value::Number(VM_MANIFEST_SCHEMA_VERSION.into()),
    );
    document.extend(manifest_mapping);

    serde_yaml::to_string(&document).context("failed to serialize VM YAML document")
}

fn capabilities(args: VmCapabilitiesArgs) -> Result<()> {
    let capabilities = query_capabilities(
        &args.connect_uri,
        args.arch.as_deref(),
        args.machine.as_deref(),
        &args.virtualization,
    )?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&capabilities)
                .context("failed to serialize VM capabilities")?
        );
        return Ok(());
    }

    let mut rows = vec![
        vec!["Domain type".to_string(), capabilities.domain_type.clone()],
        vec![
            "Architecture".to_string(),
            capabilities.architecture.clone(),
        ],
        vec![
            "Machine".to_string(),
            capabilities.machine.as_deref().unwrap_or("-").to_string(),
        ],
        vec![
            "Emulator".to_string(),
            capabilities.emulator.as_deref().unwrap_or("-").to_string(),
        ],
        vec![
            "Max vCPUs".to_string(),
            capabilities
                .max_vcpus
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
        ],
        vec![
            "Firmware".to_string(),
            join_capability_values(&capabilities.firmware),
        ],
        vec![
            "CPU modes".to_string(),
            join_capability_values(&capabilities.cpu_modes),
        ],
    ];
    rows.extend(capabilities.devices.iter().map(|device| {
        let options = device
            .options
            .iter()
            .map(|(name, values)| format!("{name}={}", values.join("|")))
            .collect::<Vec<_>>()
            .join(", ");
        vec![format!("Device: {}", device.device), options]
    }));
    crate::cli_table::print_table(&["Capability", "Value"], rows);

    Ok(())
}

fn join_capability_values(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_string()
    } else {
        values.join(", ")
    }
}

pub fn query_capabilities(
    connect_uri: &str,
    arch: Option<&str>,
    machine: Option<&str>,
    virtualization: &str,
) -> Result<VmCapabilities> {
    let conn = connect(connect_uri)?;
    let xml = conn
        .get_domain_capabilities(None, arch, machine, Some(virtualization), 0)
        .with_context(|| {
            format!("failed to query VM capabilities from libvirt at {connect_uri}")
        })?;
    parse_domain_capabilities(&xml)
}

fn parse_domain_capabilities(xml: &str) -> Result<VmCapabilities> {
    let doc = Document::parse(xml).context("failed to parse libvirt domain capabilities XML")?;
    let root = doc.root_element();
    if !root.has_tag_name("domainCapabilities") {
        bail!("expected domainCapabilities XML root");
    }

    let firmware = optional_child(root, "os")
        .and_then(|node| enum_values(node, "firmware"))
        .unwrap_or_default();
    let cpu_modes = optional_child(root, "cpu")
        .map(|cpu| {
            cpu.children()
                .filter(|node| {
                    node.has_tag_name("mode") && node.attribute("supported") == Some("yes")
                })
                .filter_map(|node| node.attribute("name").map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let devices = optional_child(root, "devices")
        .map(|devices| {
            devices
                .children()
                .filter(|node| node.is_element() && node.attribute("supported") == Some("yes"))
                .map(|node| VmDeviceCapability {
                    device: node.tag_name().name().to_string(),
                    options: node
                        .children()
                        .filter(|child| child.has_tag_name("enum"))
                        .filter_map(|child| {
                            let name = child.attribute("name")?.to_string();
                            let values = child
                                .children()
                                .filter(|value| value.has_tag_name("value"))
                                .filter_map(|value| value.text().map(str::to_string))
                                .collect();
                            Some((name, values))
                        })
                        .collect(),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(VmCapabilities {
        emulator: optional_child(root, "path")
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        domain_type: required_child_text(root, "domain")?.to_string(),
        architecture: required_child_text(root, "arch")?.to_string(),
        machine: optional_child(root, "machine")
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        max_vcpus: optional_child(root, "vcpu")
            .and_then(|node| node.attribute("max"))
            .map(str::parse)
            .transpose()
            .context("failed to parse maximum vCPU count")?,
        firmware,
        cpu_modes,
        devices,
    })
}

fn enum_values(node: Node<'_, '_>, name: &str) -> Option<Vec<String>> {
    node.children()
        .find(|child| child.has_tag_name("enum") && child.attribute("name") == Some(name))
        .map(|enumeration| {
            enumeration
                .children()
                .filter(|child| child.has_tag_name("value"))
                .filter_map(|child| child.text().map(str::to_string))
                .collect()
        })
}

fn init(args: VmInitArgs) -> Result<()> {
    let disk_paths = if args.disk.is_empty() {
        vec![PathBuf::from(format!(".tmp/disks/{}.qcow2", args.name))]
    } else {
        args.disk
    };
    let disks = disk_paths
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            VmDiskEntry::present(VmDisk {
                id: Some(format!("disk{index}")),
                disk_type: VmDiskType::File,
                path,
                format: DiskFormat::Qcow2,
                target: None,
                bus: VmDiskBus::VirtioBlk,
                cache: None,
                io: None,
                discard: None,
                detect_zeroes: None,
                readonly: None,
                serial: VmDiskSerial::default(),
                io_tune: VmDiskIoTuneConfig::default(),
            })
        })
        .collect();
    let boot = if args.no_cdrom {
        vec!["hd".to_string()]
    } else {
        vec!["cdrom".to_string(), "hd".to_string()]
    };
    let memory_mib = args
        .memory_gib
        .checked_mul(1024)
        .context("memoryGiB is too large")?;

    let manifest = VmManifest {
        name: args.name,
        machine: args.machine.map(|machine_type| VmMachine { machine_type }),
        cpu: Some(VmCpu {
            mode: VmCpuMode::HostPassthrough,
            model: None,
            vcpus: None,
            topology: Some(VmCpuTopology {
                sockets: 1,
                cores: args.vcpus,
                threads: 1,
            }),
        }),
        memory: Some(VmMemory {
            size_mib: memory_mib,
            max_mib: None,
        }),
        io_threads: None,
        disks,
        cdrom: None,
        cdroms: (!args.no_cdrom).then_some(vec![VmCdromEntry::present(VmCdrom {
            id: "installer".to_string(),
            media: Some(args.cdrom),
            target: None,
        })]),
        boot: Some(boot),
        memory_gib: args.memory_gib,
        vcpus: args.vcpus,
        network: args.network,
        graphics: GraphicsMode::Vnc,
        vnc_listen: args.vnc_listen,
        vnc_port: None,
        serial_log: None,
    };

    let yaml = serialize_manifest_yaml(&manifest).context("failed to serialize VM template")?;
    match args.output {
        Some(path) => {
            if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create directory {}", parent.display()))?;
            }
            fs::write(&path, yaml)
                .with_context(|| format!("failed to write VM YAML template {}", path.display()))?;
        }
        None => print!("{yaml}"),
    }

    Ok(())
}

fn apply(args: VmApplyArgs) -> Result<()> {
    if args.wait_shutdown && !args.start {
        bail!("--wait-shutdown requires --start");
    }
    if args.rm_after_shutdown && !args.wait_shutdown {
        bail!("--rm-after-shutdown requires --wait-shutdown");
    }

    let manifest_path = absolute_path(&args.file)?;
    let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let manifest_text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read VM definition {}", manifest_path.display()))?;
    let mut manifest = parse_manifest_yaml(&manifest_text)
        .with_context(|| format!("failed to parse VM definition {}", manifest_path.display()))?;

    normalize_manifest_paths(&mut manifest, manifest_dir)?;
    validate_manifest(&manifest)?;

    let boot = manifest_boot_order(&manifest);
    let boot_devices = domain_xml::parse_boot_devices(&boot)?;
    if boot_devices.contains(&BootDevice::Cdrom) && !manifest_has_cdrom(&manifest) {
        bail!("boot order contains cdrom but cdrom was not provided");
    }

    let current_xml = current_domain_xml(&args.connect_uri, &manifest.name)?;
    let xml = if current_xml.is_empty() {
        validate_new_vm_disks(&manifest)?;
        build_manifest_domain_xml(&manifest, &boot_devices)?
    } else {
        patch_domain_xml(&current_xml, &manifest, &boot_devices)?
    };

    if args.dry_run {
        print_apply_diff(
            &current_xml,
            &manifest.name,
            &manifest_path,
            &xml,
            should_color(args.color),
        );
        return Ok(());
    }

    prepare_serial_log_path(manifest.serial_log.as_deref())?;

    let conn = connect(&args.connect_uri)?;
    let domain = Domain::define_xml_flags(&conn, &xml, sys::VIR_DOMAIN_DEFINE_VALIDATE)
        .with_context(|| format!("failed to apply VM definition {}", manifest.name))?;

    eprintln!("[qtr] applied VM: {}", manifest.name);
    if args.start {
        start_domain(&domain, &manifest.name)?;
        if manifest.graphics == GraphicsMode::Vnc {
            print_vnc_endpoint(&domain, &manifest.vnc_listen)?;
        }
        if manifest.serial_log.is_some() {
            print_serial_log(&domain)?;
        }

        if args.wait_shutdown {
            eprintln!("[qtr] waiting for guest shutdown...");
            wait_shutdown_domain(
                &domain,
                &manifest.name,
                args.shutdown_timeout_secs.map(Duration::from_secs),
            )?;
            if args.rm_after_shutdown {
                undefine_domain(&domain, &manifest.name)?;
            }
        }

        return Ok(());
    }

    if domain
        .is_active()
        .with_context(|| format!("failed to query domain {} state", manifest.name))?
    {
        eprintln!("[qtr] VM is running; changes apply on next start");
    }

    Ok(())
}

fn build_manifest_domain_xml(manifest: &VmManifest, boot_devices: &[BootDevice]) -> Result<String> {
    let targets = assign_manifest_disk_targets(manifest)?;
    let disks = present_disks(&manifest.disks)
        .zip(targets)
        .map(|(disk, target)| launch_disk_spec(disk, target, manifest.io_threads))
        .collect::<Vec<_>>();
    let cdrom_targets = assign_manifest_cdrom_targets(manifest)?;
    let cdroms = match &manifest.cdroms {
        Some(entries) => present_cdroms(entries)
            .zip(cdrom_targets.iter())
            .map(|(cdrom, target)| domain_xml::VmLaunchCdromSpec {
                id: &cdrom.id,
                media: cdrom.media.as_deref(),
                target,
            })
            .collect::<Vec<_>>(),
        None => manifest
            .cdrom
            .as_deref()
            .map(|media| {
                vec![domain_xml::VmLaunchCdromSpec {
                    id: "installer",
                    media: Some(media),
                    target: &cdrom_targets[0],
                }]
            })
            .unwrap_or_default(),
    };

    let memory = effective_memory(manifest)?;
    let vcpus = effective_vcpus(manifest)?;

    Ok(build_vm_launch_domain_xml(VmLaunchDomainSpec {
        name: &manifest.name,
        machine: manifest
            .machine
            .as_ref()
            .map(|machine| machine.machine_type.as_str()),
        memory: memory.launch(),
        vcpus,
        cpu: launch_cpu(manifest),
        io_threads: manifest.io_threads.map(VmIoThreads::effective),
        disks: &disks,
        cdroms: &cdroms,
        serial_log: manifest.serial_log.as_deref(),
        boot_devices,
        network: &manifest.network,
        graphics: GraphicsSpec {
            mode: manifest.graphics,
            vnc_listen: &manifest.vnc_listen,
            vnc_port: manifest.vnc_port,
        },
    }))
}

fn disk_launch_source(disk_type: VmDiskType) -> VmLaunchDiskSource {
    match disk_type {
        VmDiskType::File => VmLaunchDiskSource::File,
        VmDiskType::Block => VmLaunchDiskSource::Block,
    }
}

fn launch_disk_spec(
    disk: &VmDisk,
    target: String,
    io_threads: Option<VmIoThreads>,
) -> VmLaunchDiskSpec<'_> {
    let io_threads = (disk.bus == VmDiskBus::VirtioBlk
        && disk.io.map(|io| io.mode) == Some(VmDiskIoMode::Threads))
    .then(|| io_threads.map(VmIoThreads::effective))
    .flatten();

    VmLaunchDiskSpec {
        id: disk.id.as_deref(),
        path: disk.path.clone(),
        format: disk.format,
        source: disk_launch_source(disk.disk_type),
        target,
        bus: disk.bus.target_bus().to_string(),
        cache: disk.cache.map(VmDiskCache::as_xml),
        io: disk.io.map(|io| io.mode.as_xml()),
        discard: disk.discard.map(VmDiskDiscard::as_xml),
        detect_zeroes: disk.detect_zeroes.map(VmDiskDetectZeroes::as_xml),
        readonly: disk.readonly,
        serial: disk.serial.as_deref(),
        io_tune: disk.io_tune.as_ref().copied().map(VmDiskIoTune::launch),
        io_threads,
    }
}

fn disk_target_or(disk: &VmDisk, index: usize) -> String {
    disk.target.clone().unwrap_or_else(|| match disk.bus {
        VmDiskBus::VirtioBlk => domain_xml::virtio_blk_disk_target(index),
        VmDiskBus::VirtioScsi => domain_xml::virtio_scsi_disk_target(index),
    })
}

fn assign_manifest_disk_targets(manifest: &VmManifest) -> Result<Vec<String>> {
    let mut occupied = assign_manifest_cdrom_targets(manifest)?
        .into_iter()
        .collect::<BTreeSet<_>>();

    present_disks(&manifest.disks)
        .enumerate()
        .map(|(index, disk)| {
            if let Some(target) = &disk.target {
                if target.is_empty() {
                    bail!("disk target must not be empty");
                }
                if !occupied.insert(target.clone()) {
                    bail!("duplicate disk target {target}");
                }
                return Ok(target.clone());
            }

            let mut candidate = index;
            loop {
                let target = match disk.bus {
                    VmDiskBus::VirtioBlk => domain_xml::virtio_blk_disk_target(candidate),
                    VmDiskBus::VirtioScsi => domain_xml::virtio_scsi_disk_target(candidate),
                };
                if occupied.insert(target.clone()) {
                    return Ok(target);
                }
                candidate += 1;
            }
        })
        .collect()
}

fn assign_manifest_cdrom_targets(manifest: &VmManifest) -> Result<Vec<String>> {
    let Some(entries) = &manifest.cdroms else {
        return Ok(manifest
            .cdrom
            .is_some()
            .then(|| domain_xml::CDROM_TARGET.to_string())
            .into_iter()
            .collect());
    };
    let cdroms = present_cdroms(entries).collect::<Vec<_>>();
    let mut occupied = BTreeSet::new();
    let mut targets = vec![None; cdroms.len()];
    for (index, cdrom) in cdroms.iter().enumerate() {
        if let Some(target) = cdrom.target.as_deref() {
            validate_cdrom_target(target)?;
            if !occupied.insert(target.to_string()) {
                bail!("duplicate CD-ROM target {target}");
            }
            targets[index] = Some(target.to_string());
        }
    }
    for target in &mut targets {
        if target.is_none() {
            for index in 0.. {
                let candidate = domain_xml::virtio_scsi_disk_target(index);
                if occupied.insert(candidate.clone()) {
                    *target = Some(candidate);
                    break;
                }
            }
        }
    }
    Ok(targets
        .into_iter()
        .map(|target| target.expect("CD-ROM target planning should be complete"))
        .collect())
}

fn present_disks(disks: &[VmDiskEntry]) -> impl Iterator<Item = &VmDisk> {
    disks.iter().filter_map(VmDiskEntry::as_present)
}

fn present_cdroms(cdroms: &[VmCdromEntry]) -> impl Iterator<Item = &VmCdrom> {
    cdroms.iter().filter_map(VmCdromEntry::as_present)
}

fn manifest_has_cdrom(manifest: &VmManifest) -> bool {
    match &manifest.cdroms {
        Some(cdroms) => present_cdroms(cdroms).next().is_some(),
        None => manifest.cdrom.is_some(),
    }
}

fn validate_new_vm_disks(manifest: &VmManifest) -> Result<()> {
    if present_disks(&manifest.disks).next().is_none() {
        bail!("new VM definition must contain at least one present disk");
    }
    Ok(())
}

fn dump(args: VmDumpArgs) -> Result<()> {
    let xml = existing_domain_xml(&args.connect_uri, &args.name)?;
    let output = if args.xml {
        xml
    } else {
        let manifest = manifest_from_domain_xml(&xml)?;
        serialize_manifest_yaml(&manifest)
            .with_context(|| format!("failed to serialize VM {} as YAML", args.name))?
    };

    match args.output {
        Some(path) => {
            if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create directory {}", parent.display()))?;
            }
            fs::write(&path, output)
                .with_context(|| format!("failed to write VM dump {}", path.display()))?;
        }
        None => print!("{output}"),
    }

    Ok(())
}

fn manifest_from_domain_xml(xml: &str) -> Result<VmManifest> {
    let doc = Document::parse(xml).context("failed to parse libvirt domain XML")?;
    let domain = doc.root_element();

    let name = required_child_text(domain, "name")?.to_string();
    let memory = memory_from_domain_xml(domain)?;
    let vcpus = required_child_text(domain, "vcpu")?
        .parse()
        .context("failed to parse domain vcpus")?;

    let boot = boot_order(domain)?;
    let devices = required_child(domain, "devices")?;
    let disks = disks_from_domain_xml(devices)?
        .into_iter()
        .map(VmDiskEntry::present)
        .collect();
    let io_threads = io_threads_from_domain_xml(domain, devices)?;
    let cdroms = cdroms_from_domain_xml(devices)?;
    let network = network_name(devices)?;
    let (graphics, vnc_listen, vnc_port) = graphics_config(devices)?;
    let serial_log = serial_log_path(devices);

    Ok(VmManifest {
        name,
        machine: machine_from_domain_xml(domain),
        cpu: cpu_from_domain_xml(domain, vcpus)?,
        memory: Some(memory),
        io_threads,
        disks,
        cdrom: None,
        cdroms: (!cdroms.is_empty()).then_some(cdroms),
        boot: Some(boot),
        memory_gib: memory.size_mib / 1024,
        vcpus,
        network,
        graphics,
        vnc_listen,
        vnc_port,
        serial_log,
    })
}

struct XmlReplacement {
    range: Range<usize>,
    value: String,
}

fn patch_domain_xml(
    xml: &str,
    manifest: &VmManifest,
    boot_devices: &[BootDevice],
) -> Result<String> {
    let doc = Document::parse(xml).context("failed to parse existing libvirt domain XML")?;
    let domain = doc.root_element();
    let devices = required_child(domain, "devices")?;
    let mut replacements = Vec::new();
    let memory = effective_memory(manifest)?;
    let vcpus = effective_vcpus(manifest)?;

    patch_memory(xml, domain, memory, &mut replacements)?;
    patch_io_threads(xml, domain, manifest.io_threads, &mut replacements)?;
    push_text_replacement(
        xml,
        required_child(domain, "vcpu")?,
        &vcpus.to_string(),
        &mut replacements,
    )?;
    patch_machine(xml, domain, manifest.machine.as_ref(), &mut replacements)?;
    patch_cpu(xml, domain, manifest.cpu.as_ref(), &mut replacements)?;
    patch_boot_order(xml, domain, boot_devices, &mut replacements)?;
    let mut reserved_cdrom_targets = devices
        .children()
        .filter(|child| child.has_tag_name("disk") && child.attribute("device") == Some("cdrom"))
        .filter_map(disk_target_dev)
        .collect::<BTreeSet<_>>();
    reserved_cdrom_targets.extend(assign_manifest_cdrom_targets(manifest)?);
    patch_disks(
        xml,
        devices,
        &manifest.disks,
        manifest.io_threads,
        &reserved_cdrom_targets,
        &mut replacements,
    )?;
    let present_manifest_disks = present_disks(&manifest.disks).cloned().collect::<Vec<_>>();
    patch_virtio_scsi_controller(
        xml,
        devices,
        &present_manifest_disks,
        manifest.io_threads,
        &mut replacements,
    );

    if let Some(cdrom) = &manifest.cdrom {
        patch_disk_source(xml, devices, "cdrom", None, cdrom, &mut replacements)?;
    } else if let Some(cdroms) = &manifest.cdroms {
        patch_cdroms(xml, devices, cdroms, &mut replacements)?;
    }

    patch_network(xml, devices, &manifest.network, &mut replacements)?;
    patch_graphics(xml, devices, manifest, &mut replacements)?;

    if let Some(serial_log) = &manifest.serial_log {
        patch_serial_log(xml, devices, serial_log, &mut replacements)?;
    }

    let output = apply_xml_replacements(xml, replacements)?;
    validate_unique_domain_disk_targets(&output)?;
    Ok(output)
}

fn validate_unique_domain_disk_targets(xml: &str) -> Result<()> {
    let doc = Document::parse(xml).context("failed to parse reconciled domain XML")?;
    let devices = required_child(doc.root_element(), "devices")?;
    let mut targets = BTreeSet::new();
    for target in devices
        .children()
        .filter(|child| child.has_tag_name("disk"))
        .filter_map(disk_target_dev)
    {
        if !targets.insert(target.clone()) {
            bail!("reconciled domain XML has duplicate disk target {target}");
        }
    }
    Ok(())
}

fn patch_disks(
    xml: &str,
    devices: Node<'_, '_>,
    manifest_disks: &[VmDiskEntry],
    io_threads: Option<VmIoThreads>,
    reserved_cdrom_targets: &BTreeSet<String>,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let domain_disk_nodes = devices
        .children()
        .filter(|child| child.has_tag_name("disk") && child.attribute("device") == Some("disk"))
        .collect::<Vec<_>>();
    let domain_disks = disks_from_domain_xml(devices)?;
    let present_disks = present_disks(manifest_disks).cloned().collect::<Vec<_>>();
    validate_implicit_disk_identities(&present_disks)?;

    let mut domain_disks = domain_disk_nodes
        .into_iter()
        .zip(domain_disks)
        .map(|(node, disk)| DomainDiskPatchEntry {
            node,
            disk,
            used: false,
        })
        .collect::<Vec<_>>();
    for id in manifest_disks.iter().filter_map(VmDiskEntry::absent_id) {
        let matches = domain_disks
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.used && entry.disk.id.as_deref() == Some(id))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            bail!("existing domain XML has duplicate disk id {id}");
        }
        let Some(index) = matches.into_iter().next() else {
            continue;
        };
        domain_disks[index].used = true;
        let range = domain_disks[index].node.range();
        replacements.push(XmlReplacement {
            range: line_start(xml, range.start)..line_end(xml, range.end),
            value: String::new(),
        });
    }

    let mut plans = Vec::with_capacity(present_disks.len());
    for (index, disk) in present_disks.iter().enumerate() {
        let matched_index = find_domain_disk_match(&domain_disks, disk)?;
        if let Some(matched_index) = matched_index {
            domain_disks[matched_index].used = true;
        }
        plans.push((index, disk, matched_index, None::<String>));
    }

    if let Some(unmatched) = domain_disks.iter().find(|entry| !entry.used) {
        let target = unmatched.disk.target.as_deref().unwrap_or("unknown");
        bail!(
            "manifest does not identify existing disk target {target}; keep it by stable id or target, or add state: absent to detach it"
        );
    }

    let mut output_targets = reserved_cdrom_targets.clone();
    for (_, disk, matched_index, target) in &mut plans {
        let requested = disk
            .target
            .as_deref()
            .or_else(|| matched_index.and_then(|index| domain_disks[index].disk.target.as_deref()));
        if let Some(requested) = requested {
            validate_disk_target_for_bus(requested, disk.bus)?;
            if !output_targets.insert(requested.to_string()) {
                bail!("duplicate disk target {requested}");
            }
            *target = Some(requested.to_string());
        }
    }
    for (_, disk, _, target) in &mut plans {
        if target.is_none() {
            *target = Some(next_available_disk_target(
                disk.bus,
                &mut output_targets,
                &BTreeSet::new(),
            ));
        }
    }

    let mut new_disks_xml = String::new();
    for (index, manifest_disk, matched_index, target) in plans {
        let target = target.expect("disk target planning should be complete");
        let mut desired_disk = manifest_disk.clone();
        desired_disk.target = Some(target.clone());
        if let Some(matched_index) = matched_index {
            domain_disks[matched_index].used = true;
            let domain_disk = domain_disks[matched_index].node;
            let range = domain_disk.range();
            let start = line_start(xml, range.start);
            let end = line_end(xml, range.end);
            let desired =
                build_patched_disk_xml(xml, domain_disk, &desired_disk, index, io_threads);
            replacements.push(XmlReplacement {
                range: start..end,
                value: desired,
            });
        } else {
            new_disks_xml.push_str(&domain_xml::build_disk_xml(&launch_disk_spec(
                &desired_disk,
                target,
                io_threads,
            )));
        }
    }

    let needs_scsi_controller = present_disks
        .iter()
        .any(|disk| disk.bus == VmDiskBus::VirtioScsi)
        && !has_virtio_scsi_controller(devices);
    if needs_scsi_controller {
        new_disks_xml.push_str(&domain_xml::build_virtio_scsi_controller_xml(
            virtio_scsi_controller_io_threads(&present_disks, io_threads),
        ));
    }

    if !new_disks_xml.is_empty() {
        let end = domain_disks
            .last()
            .map(|entry| line_end(xml, entry.node.range().end))
            .or_else(|| {
                optional_child(devices, "emulator").map(|node| line_end(xml, node.range().end))
            })
            .or_else(|| {
                devices
                    .children()
                    .find(Node::is_element)
                    .map(|node| line_start(xml, node.range().start))
            })
            .unwrap_or_else(|| devices_closing_tag_start(xml, devices));
        replacements.push(XmlReplacement {
            range: end..end,
            value: new_disks_xml,
        });
    }

    Ok(())
}

fn devices_closing_tag_start(xml: &str, devices: Node<'_, '_>) -> usize {
    let range = devices.range();
    xml[range.clone()]
        .rfind("</devices>")
        .map(|offset| line_start(xml, range.start + offset))
        .expect("parsed devices element should have a closing tag")
}

fn patch_cdroms(
    xml: &str,
    devices: Node<'_, '_>,
    manifest_cdroms: &[VmCdromEntry],
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let domain_nodes = devices
        .children()
        .filter(|child| child.has_tag_name("disk") && child.attribute("device") == Some("cdrom"))
        .collect::<Vec<_>>();
    let domain_cdroms = cdroms_from_domain_xml(devices)?;
    let mut domain_cdroms = domain_nodes
        .into_iter()
        .zip(domain_cdroms)
        .map(|(node, entry)| DomainCdromPatchEntry {
            node,
            cdrom: entry
                .as_present()
                .expect("domain CD-ROM parser returns present entries")
                .clone(),
            used: false,
        })
        .collect::<Vec<_>>();

    for id in manifest_cdroms.iter().filter_map(VmCdromEntry::absent_id) {
        let matches = domain_cdroms
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.used && entry.cdrom.id == id)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            bail!("existing domain XML has duplicate CD-ROM id {id}");
        }
        let Some(index) = matches.into_iter().next() else {
            continue;
        };
        domain_cdroms[index].used = true;
        let range = domain_cdroms[index].node.range();
        replacements.push(XmlReplacement {
            range: line_start(xml, range.start)..line_end(xml, range.end),
            value: String::new(),
        });
    }

    let desired = present_cdroms(manifest_cdroms).collect::<Vec<_>>();
    let mut plans = Vec::with_capacity(desired.len());
    for cdrom in desired {
        let matches = domain_cdroms
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                !entry.used
                    && (entry.cdrom.id == cdrom.id
                        || cdrom.target.as_deref() == entry.cdrom.target.as_deref())
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            bail!(
                "existing domain XML has multiple CD-ROMs matching {}",
                cdrom.id
            );
        }
        let matched = matches.into_iter().next();
        if let Some(index) = matched {
            domain_cdroms[index].used = true;
        }
        plans.push((cdrom, matched, None::<String>));
    }

    let mut occupied = devices
        .children()
        .filter(|child| child.has_tag_name("disk") && child.attribute("device") == Some("disk"))
        .filter_map(disk_target_dev)
        .chain(
            domain_cdroms
                .iter()
                .filter(|entry| !entry.used)
                .filter_map(|entry| entry.cdrom.target.clone()),
        )
        .collect::<BTreeSet<_>>();
    for (cdrom, matched, target) in &mut plans {
        let requested = cdrom
            .target
            .as_deref()
            .or_else(|| matched.and_then(|index| domain_cdroms[index].cdrom.target.as_deref()));
        if let Some(requested) = requested {
            validate_cdrom_target(requested)?;
            if !occupied.insert(requested.to_string()) {
                bail!("CD-ROM target {requested} is already in use");
            }
            *target = Some(requested.to_string());
        }
    }
    for (_, _, target) in &mut plans {
        if target.is_none() {
            for index in 0.. {
                let candidate = domain_xml::virtio_scsi_disk_target(index);
                if occupied.insert(candidate.clone()) {
                    *target = Some(candidate);
                    break;
                }
            }
        }
    }

    let mut new_xml = String::new();
    for (cdrom, matched, target) in plans {
        let target = target.expect("CD-ROM target planning should be complete");
        let desired_xml = domain_xml::build_cdrom_xml(&domain_xml::VmLaunchCdromSpec {
            id: &cdrom.id,
            media: cdrom.media.as_deref(),
            target: &target,
        });
        if let Some(index) = matched {
            let node = domain_cdroms[index].node;
            let range = node.range();
            replacements.push(XmlReplacement {
                range: line_start(xml, range.start)..line_end(xml, range.end),
                value: vm_reconcile::merge_cdrom_xml(xml, node, &desired_xml),
            });
        } else {
            new_xml.push_str(&desired_xml);
        }
    }

    if !new_xml.is_empty() {
        let end = devices
            .children()
            .rfind(|child| child.has_tag_name("disk"))
            .map(|node| line_end(xml, node.range().end))
            .or_else(|| {
                optional_child(devices, "emulator").map(|node| line_end(xml, node.range().end))
            })
            .or_else(|| {
                devices
                    .children()
                    .find(Node::is_element)
                    .map(|node| line_start(xml, node.range().start))
            })
            .unwrap_or_else(|| devices_closing_tag_start(xml, devices));
        replacements.push(XmlReplacement {
            range: end..end,
            value: new_xml,
        });
    }

    Ok(())
}

struct DomainCdromPatchEntry<'a, 'input> {
    node: Node<'a, 'input>,
    cdrom: VmCdrom,
    used: bool,
}

fn patch_virtio_scsi_controller(
    xml: &str,
    devices: Node<'_, '_>,
    manifest_disks: &[VmDisk],
    io_threads: Option<VmIoThreads>,
    replacements: &mut Vec<XmlReplacement>,
) {
    let Some(controller) = virtio_scsi_controller(devices) else {
        return;
    };
    let controller_io_threads = manifest_disks
        .iter()
        .any(|disk| disk.bus == VmDiskBus::VirtioScsi)
        .then(|| virtio_scsi_controller_io_threads(manifest_disks, io_threads))
        .flatten();

    let range = controller.range();
    let start = line_start(xml, range.start);
    let end = line_end(xml, range.end);
    replacements.push(XmlReplacement {
        range: start..end,
        value: domain_xml::build_virtio_scsi_controller_xml(controller_io_threads),
    });
}

fn virtio_scsi_controller_io_threads(
    disks: &[VmDisk],
    io_threads: Option<VmIoThreads>,
) -> Option<VmLaunchIoThreadsSpec> {
    disks
        .iter()
        .any(|disk| {
            disk.bus == VmDiskBus::VirtioScsi
                && disk.io.map(|io| io.mode) == Some(VmDiskIoMode::Threads)
        })
        .then(|| io_threads.map(VmIoThreads::effective))
        .flatten()
}

struct DomainDiskPatchEntry<'a, 'input> {
    node: Node<'a, 'input>,
    disk: VmDisk,
    used: bool,
}

fn validate_implicit_disk_identities(disks: &[VmDisk]) -> Result<()> {
    for (index, disk) in disks.iter().enumerate() {
        if disk.target.is_some() {
            continue;
        }
        if disks[index + 1..]
            .iter()
            .any(|other| other.target.is_none() && same_disk_identity(disk, other))
        {
            bail!(
                "disk {} appears more than once without explicit target",
                disk.path.display()
            );
        }
    }

    Ok(())
}

fn find_domain_disk_match(
    domain_disks: &[DomainDiskPatchEntry<'_, '_>],
    manifest_disk: &VmDisk,
) -> Result<Option<usize>> {
    if let Some(id) = manifest_disk.id.as_deref() {
        let matches = domain_disks
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.used && entry.disk.id.as_deref() == Some(id))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            bail!("existing domain XML has duplicate disk id {id}");
        }
        if let Some(index) = matches.into_iter().next() {
            return Ok(Some(index));
        }
    }

    if let Some(target) = manifest_disk.target.as_deref() {
        let matches = domain_disks
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.used && entry.disk.target.as_deref() == Some(target))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            bail!("existing domain XML has duplicate disk target {target}");
        }
        if let Some(index) = matches.into_iter().next() {
            return Ok(Some(index));
        }
    }

    let matches = domain_disks
        .iter()
        .enumerate()
        .filter(|(_, entry)| !entry.used && same_disk_identity(&entry.disk, manifest_disk))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        bail!(
            "existing domain XML has multiple disks for {}; set target explicitly",
            manifest_disk.path.display()
        );
    }

    Ok(matches.into_iter().next())
}

fn same_disk_identity(left: &VmDisk, right: &VmDisk) -> bool {
    left.disk_type == right.disk_type && left.path == right.path
}

fn next_available_disk_target(
    bus: VmDiskBus,
    occupied_targets: &mut BTreeSet<String>,
    output_targets: &BTreeSet<String>,
) -> String {
    for index in 0.. {
        let target = match bus {
            VmDiskBus::VirtioBlk => domain_xml::virtio_blk_disk_target(index),
            VmDiskBus::VirtioScsi => domain_xml::virtio_scsi_disk_target(index),
        };
        if !occupied_targets.contains(&target) && !output_targets.contains(&target) {
            occupied_targets.insert(target.clone());
            return target;
        }
    }

    unreachable!("unbounded disk target search should always find a target")
}

fn validate_disk_target_for_bus(target: &str, bus: VmDiskBus) -> Result<()> {
    match bus {
        VmDiskBus::VirtioBlk if !target.starts_with("vd") => {
            bail!("virtio-blk disk target {target} must start with vd")
        }
        VmDiskBus::VirtioScsi if !target.starts_with("sd") => {
            bail!("virtio-scsi disk target {target} must start with sd")
        }
        _ => Ok(()),
    }
}

fn patch_io_threads(
    xml: &str,
    domain: Node<'_, '_>,
    io_threads: Option<VmIoThreads>,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    match (optional_child(domain, "iothreads"), io_threads) {
        (Some(node), Some(io_threads)) => {
            push_text_replacement(xml, node, &io_threads.count.to_string(), replacements)?;
        }
        (None, Some(io_threads)) => {
            let vcpu = required_child(domain, "vcpu")?;
            let range = vcpu.range();
            let end = line_end(xml, range.end);
            replacements.push(XmlReplacement {
                range: end..end,
                value: format!("  <iothreads>{}</iothreads>\n", io_threads.count),
            });
        }
        (Some(node), None) => {
            let range = node.range();
            replacements.push(XmlReplacement {
                range: line_start(xml, range.start)..line_end(xml, range.end),
                value: String::new(),
            });
        }
        (None, None) => {}
    }

    Ok(())
}

fn build_patched_disk_xml(
    xml: &str,
    domain_disk: Node<'_, '_>,
    manifest_disk: &VmDisk,
    index: usize,
    io_threads: Option<VmIoThreads>,
) -> String {
    let desired_xml = domain_xml::build_disk_xml(&launch_disk_spec(
        manifest_disk,
        disk_target_or(manifest_disk, index),
        io_threads,
    ));
    vm_reconcile::merge_disk_xml(
        xml,
        domain_disk,
        &desired_xml,
        !manifest_disk.io_tune.is_preserve(),
        manifest_disk.readonly.is_some(),
        !manifest_disk.serial.is_preserve(),
    )
}

fn patch_machine(
    xml: &str,
    domain: Node<'_, '_>,
    machine: Option<&VmMachine>,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let Some(machine) = machine else {
        return Ok(());
    };
    let os_type = required_child(required_child(domain, "os")?, "type")?;
    push_attr_upsert_replacement(xml, os_type, "machine", &machine.machine_type, replacements)
}

fn patch_cpu(
    xml: &str,
    domain: Node<'_, '_>,
    cpu: Option<&VmCpu>,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let Some(cpu) = cpu else {
        return Ok(());
    };
    let spec = VmLaunchCpuSpec {
        mode: cpu.mode.as_xml(),
        model: cpu.model.as_deref(),
        topology: cpu.topology.map(VmCpuTopology::launch),
    };

    if let Some(current) = optional_child(domain, "cpu") {
        let range = current.range();
        replacements.push(XmlReplacement {
            range: line_start(xml, range.start)..line_end(xml, range.end),
            value: build_patched_cpu_xml(xml, current, spec),
        });
    } else {
        let devices = required_child(domain, "devices")?;
        let start = line_start(xml, devices.range().start);
        replacements.push(XmlReplacement {
            range: start..start,
            value: domain_xml::build_cpu_xml(spec),
        });
    }

    Ok(())
}

fn build_patched_cpu_xml(xml: &str, current: Node<'_, '_>, spec: VmLaunchCpuSpec<'_>) -> String {
    let mut desired = domain_xml::build_cpu_xml(spec);
    let extra_attributes = current
        .attributes()
        .filter(|attribute| !matches!(attribute.name(), "mode" | "check" | "migratable" | "match"))
        .map(|attribute| {
            format!(
                " {}='{}'",
                attribute.name(),
                escape_xml_value(attribute.value())
            )
        })
        .collect::<String>();
    if !extra_attributes.is_empty() {
        let tag_end = desired.find('>').expect("generated CPU XML has start tag");
        let insert_at = if desired.as_bytes()[tag_end - 1] == b'/' {
            tag_end - 1
        } else {
            tag_end
        };
        desired.insert_str(insert_at, &extra_attributes);
    }

    let extra_children = current
        .children()
        .filter(|child| {
            child.is_element() && !child.has_tag_name("model") && !child.has_tag_name("topology")
        })
        .map(|child| {
            let range = child.range();
            (child.tag_name().name(), format!("    {}\n", &xml[range]))
        })
        .collect::<Vec<_>>();
    if extra_children.is_empty() {
        return desired;
    }

    if !desired.contains("  </cpu>\n") {
        let self_close = desired
            .rfind("/>\n")
            .expect("generated CPU XML is self-closing or has a closing tag");
        desired.replace_range(self_close.., ">\n  </cpu>\n");
    }

    let before_topology = extra_children
        .iter()
        .filter(|(name, _)| *name == "vendor")
        .map(|(_, xml)| xml.as_str())
        .collect::<String>();
    if !before_topology.is_empty() {
        let insert_at = desired
            .find("    <topology")
            .or_else(|| desired.rfind("  </cpu>\n"))
            .expect("expanded CPU XML has a closing tag");
        desired.insert_str(insert_at, &before_topology);
    }

    let after_topology = extra_children
        .iter()
        .filter(|(name, _)| *name != "vendor")
        .map(|(_, xml)| xml.as_str())
        .collect::<String>();
    if !after_topology.is_empty() {
        let close = desired
            .rfind("  </cpu>\n")
            .expect("expanded CPU XML has a closing tag");
        desired.insert_str(close, &after_topology);
    }

    desired
}

fn patch_memory(
    xml: &str,
    domain: Node<'_, '_>,
    memory: VmMemory,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let max_memory_mib = memory.max_mib.unwrap_or(memory.size_mib);
    let max_memory = required_child(domain, "memory")?;
    patch_memory_element(xml, max_memory, max_memory_mib, replacements)?;

    if let Some(current_memory) = optional_child(domain, "currentMemory") {
        patch_memory_element(xml, current_memory, memory.size_mib, replacements)?;
    } else {
        let end = line_end(xml, max_memory.range().end);
        replacements.push(XmlReplacement {
            range: end..end,
            value: format!(
                "  <currentMemory unit='MiB'>{}</currentMemory>\n",
                memory.size_mib
            ),
        });
    }

    Ok(())
}

fn patch_memory_element(
    xml: &str,
    node: Node<'_, '_>,
    memory_mib: u64,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let unit = node.attribute("unit").unwrap_or("KiB");
    if unit == "GiB" && !memory_mib.is_multiple_of(1024) {
        push_attr_upsert_replacement(xml, node, "unit", "MiB", replacements)?;
        return push_text_replacement(xml, node, &memory_mib.to_string(), replacements);
    }

    let value = memory_value_for_unit(memory_mib, unit)?;
    push_text_replacement(xml, node, &value.to_string(), replacements)
}

fn memory_value_for_unit(memory_mib: u64, unit: &str) -> Result<u64> {
    match unit {
        "KiB" => memory_mib
            .checked_mul(1024)
            .context("memory is too large for KiB domain memory"),
        "MiB" => Ok(memory_mib),
        "GiB" => {
            if !memory_mib.is_multiple_of(1024) {
                bail!("memory cannot be represented as whole GiB in existing domain XML");
            }
            Ok(memory_mib / 1024)
        }
        _ => bail!("unsupported domain memory unit {unit:?}"),
    }
}

fn patch_boot_order(
    xml: &str,
    domain: Node<'_, '_>,
    boot_devices: &[BootDevice],
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let os = required_child(domain, "os")?;
    let boot_nodes = os
        .children()
        .filter(|child| child.has_tag_name("boot"))
        .collect::<Vec<_>>();
    if boot_nodes.is_empty() {
        bail!("cannot update existing domain XML because <os> has no <boot> entries");
    }

    let current_boot = boot_order(domain)?;
    let desired_boot = boot_devices
        .iter()
        .map(|device| boot_device_name(*device).to_string())
        .collect::<Vec<_>>();
    if current_boot == desired_boot {
        return Ok(());
    }

    let first = boot_nodes.first().expect("checked non-empty").range();
    let last = boot_nodes.last().expect("checked non-empty").range();
    let start = line_start(xml, first.start);
    let end = line_end(xml, last.end);
    let indent = &xml[start..first.start];
    let value = boot_devices
        .iter()
        .map(|device| format!("{indent}<boot dev='{}'/>\n", boot_device_name(*device)))
        .collect::<String>();

    replacements.push(XmlReplacement {
        range: start..end,
        value,
    });

    Ok(())
}

fn patch_disk_source(
    xml: &str,
    devices: Node<'_, '_>,
    device: &str,
    target_dev: Option<&str>,
    path: &Path,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let disk = find_disk(devices, device, target_dev)?.with_context(|| {
        let target = target_dev
            .map(|target| format!(" target {target}"))
            .unwrap_or_default();
        format!("cannot update existing domain XML because {device} disk{target} is missing")
    })?;
    let source =
        optional_child(disk, "source").context("domain XML disk is missing source element")?;
    push_attr_replacement(
        xml,
        source,
        "file",
        &path.display().to_string(),
        replacements,
    )
}

fn patch_network(
    xml: &str,
    devices: Node<'_, '_>,
    network: &str,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let interface = devices
        .children()
        .find(|child| child.has_tag_name("interface") && child.attribute("type") == Some("network"))
        .context("cannot update existing domain XML because network interface is missing")?;
    let source = optional_child(interface, "source")
        .context("domain XML network interface is missing source element")?;
    push_attr_replacement(xml, source, "network", network, replacements)
}

fn patch_graphics(
    xml: &str,
    devices: Node<'_, '_>,
    manifest: &VmManifest,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    match manifest.graphics {
        GraphicsMode::None => {
            if devices
                .children()
                .any(|child| child.has_tag_name("graphics"))
            {
                bail!("cannot remove graphics from an existing domain XML yet");
            }
        }
        GraphicsMode::Vnc => {
            let graphics = devices
                .children()
                .find(|child| child.has_tag_name("graphics"))
                .context("cannot update existing domain XML because VNC graphics is missing")?;
            if graphics.attribute("type") != Some("vnc") {
                bail!("cannot update non-VNC graphics in existing domain XML");
            }

            if graphics.attribute("listen").is_some() {
                push_attr_replacement(xml, graphics, "listen", &manifest.vnc_listen, replacements)?;
            }
            if let Some(listen) = optional_child(graphics, "listen")
                && listen.attribute("address").is_some()
            {
                push_attr_replacement(xml, listen, "address", &manifest.vnc_listen, replacements)?;
            }

            let port = manifest
                .vnc_port
                .map(|port| port.to_string())
                .unwrap_or_else(|| "-1".to_string());
            let autoport = if manifest.vnc_port.is_some() {
                "no"
            } else {
                "yes"
            };
            if graphics.attribute("port").is_some() {
                push_attr_replacement(xml, graphics, "port", &port, replacements)?;
            }
            if graphics.attribute("autoport").is_some() {
                push_attr_replacement(xml, graphics, "autoport", autoport, replacements)?;
            }
        }
    }

    Ok(())
}

fn patch_serial_log(
    xml: &str,
    devices: Node<'_, '_>,
    path: &Path,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let desired = path.display().to_string();
    let mut patched = false;

    for console in devices
        .children()
        .filter(|child| child.has_tag_name("console") && child.attribute("type") == Some("file"))
    {
        if let Some(source) = optional_child(console, "source")
            && source.attribute("path").is_some()
        {
            push_attr_replacement(xml, source, "path", &desired, replacements)?;
            patched = true;
        }
    }

    for serial in devices
        .children()
        .filter(|child| child.has_tag_name("serial") && child.attribute("type") == Some("file"))
    {
        if let Some(source) = optional_child(serial, "source")
            && source.attribute("path").is_some()
        {
            push_attr_replacement(xml, source, "path", &desired, replacements)?;
            patched = true;
        }
    }

    if !patched {
        bail!("cannot update existing domain XML because file console/serial log is missing");
    }

    Ok(())
}

fn find_disk<'a, 'input>(
    devices: Node<'a, 'input>,
    device: &str,
    target_dev: Option<&str>,
) -> Result<Option<Node<'a, 'input>>> {
    for disk in devices
        .children()
        .filter(|child| child.has_tag_name("disk") && child.attribute("device") == Some(device))
    {
        if let Some(target_dev) = target_dev
            && disk_target_dev(disk).as_deref() != Some(target_dev)
        {
            continue;
        }

        return Ok(Some(disk));
    }

    Ok(None)
}

fn push_text_replacement(
    xml: &str,
    node: Node<'_, '_>,
    value: &str,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let range = node_text_range(node)?;
    if &xml[range.clone()] != value {
        replacements.push(XmlReplacement {
            range,
            value: value.to_string(),
        });
    }

    Ok(())
}

fn push_attr_replacement(
    xml: &str,
    node: Node<'_, '_>,
    attr_name: &str,
    value: &str,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let attr = node
        .attributes()
        .find(|attr| attr.name() == attr_name)
        .with_context(|| {
            format!(
                "domain XML <{}> is missing {attr_name} attribute",
                node.tag_name().name()
            )
        })?;
    let range = attr.range_value();
    let escaped = escape_xml_value(value);
    if xml[range.clone()] != *escaped {
        replacements.push(XmlReplacement {
            range,
            value: escaped,
        });
    }

    Ok(())
}

fn push_attr_upsert_replacement(
    xml: &str,
    node: Node<'_, '_>,
    attr_name: &str,
    value: &str,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    if node.attribute(attr_name).is_some() {
        return push_attr_replacement(xml, node, attr_name, value, replacements);
    }

    let start = node.range().start;
    let tag_end = xml[start..]
        .find('>')
        .map(|offset| start + offset)
        .with_context(|| format!("domain XML <{}> has no end bracket", node.tag_name().name()))?;
    let insert_at = if xml.as_bytes().get(tag_end.saturating_sub(1)) == Some(&b'/') {
        tag_end - 1
    } else {
        tag_end
    };
    replacements.push(XmlReplacement {
        range: insert_at..insert_at,
        value: format!(" {attr_name}='{}'", escape_xml_value(value)),
    });

    Ok(())
}

fn node_text_range(node: Node<'_, '_>) -> Result<Range<usize>> {
    node.children()
        .find(|child| child.is_text())
        .map(|child| child.range())
        .with_context(|| format!("domain XML <{}> is missing text", node.tag_name().name()))
}

fn apply_xml_replacements(xml: &str, mut replacements: Vec<XmlReplacement>) -> Result<String> {
    replacements.sort_by_key(|replacement| replacement.range.start);
    for replacement in &replacements {
        if replacement.range.end > xml.len() || replacement.range.start > replacement.range.end {
            bail!("invalid domain XML replacement range");
        }
    }
    for pair in replacements.windows(2) {
        if pair[0].range.end > pair[1].range.start {
            bail!("overlapping domain XML replacements");
        }
    }

    let mut output = xml.to_string();
    for replacement in replacements.into_iter().rev() {
        output.replace_range(replacement.range, &replacement.value);
    }

    Ok(output)
}

fn line_start(xml: &str, pos: usize) -> usize {
    xml[..pos].rfind('\n').map(|index| index + 1).unwrap_or(0)
}

fn line_end(xml: &str, pos: usize) -> usize {
    xml[pos..]
        .find('\n')
        .map(|index| pos + index + 1)
        .unwrap_or(xml.len())
}

fn boot_device_name(device: BootDevice) -> &'static str {
    match device {
        BootDevice::Hd => "hd",
        BootDevice::Cdrom => "cdrom",
    }
}

fn escape_xml_value(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn required_child<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Result<Node<'a, 'input>> {
    node.children()
        .find(|child| child.has_tag_name(tag))
        .with_context(|| format!("domain XML is missing <{tag}>"))
}

fn optional_child<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|child| child.has_tag_name(tag))
}

fn required_child_text<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Result<&'a str> {
    required_child(node, tag)?
        .text()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("domain XML <{tag}> is empty"))
}

fn memory_element_mib(memory: Node<'_, '_>) -> Result<u64> {
    let value = memory
        .text()
        .map(str::trim)
        .context("domain XML <memory> is empty")?
        .parse::<u64>()
        .context("failed to parse domain memory")?;

    match memory.attribute("unit").unwrap_or("KiB") {
        "KiB" => {
            if value % 1024 != 0 {
                bail!("domain memory {value} KiB cannot be represented as whole MiB");
            }
            Ok(value / 1024)
        }
        "MiB" => Ok(value),
        "GiB" => value
            .checked_mul(1024)
            .context("domain memory is too large"),
        unit => bail!("unsupported domain memory unit {unit:?}"),
    }
}

fn memory_from_domain_xml(domain: Node<'_, '_>) -> Result<VmMemory> {
    let max_mib = memory_element_mib(required_child(domain, "memory")?)?;
    let size_mib = optional_child(domain, "currentMemory")
        .map(memory_element_mib)
        .transpose()?
        .unwrap_or(max_mib);

    Ok(VmMemory {
        size_mib,
        max_mib: (max_mib != size_mib).then_some(max_mib),
    })
}

fn memory_mib(domain: Node<'_, '_>) -> Result<u64> {
    Ok(memory_from_domain_xml(domain)?.size_mib)
}

fn machine_from_domain_xml(domain: Node<'_, '_>) -> Option<VmMachine> {
    optional_child(domain, "os")
        .and_then(|os| optional_child(os, "type"))
        .and_then(|os_type| os_type.attribute("machine"))
        .map(|machine_type| VmMachine {
            machine_type: machine_type.to_string(),
        })
}

fn cpu_from_domain_xml(domain: Node<'_, '_>, vcpus: u32) -> Result<Option<VmCpu>> {
    let Some(cpu) = optional_child(domain, "cpu") else {
        return Ok(None);
    };
    let mode = match cpu.attribute("mode").unwrap_or("custom") {
        "host-passthrough" => VmCpuMode::HostPassthrough,
        "host-model" => VmCpuMode::HostModel,
        "custom" => VmCpuMode::Custom,
        mode => bail!("unsupported CPU mode {mode:?}"),
    };
    let model = if mode == VmCpuMode::Custom {
        Some(required_child_text(cpu, "model")?.to_string())
    } else {
        None
    };
    let topology = optional_child(cpu, "topology")
        .map(|topology| -> Result<VmCpuTopology> {
            if let Some(attribute) = topology
                .attributes()
                .find(|attribute| !matches!(attribute.name(), "sockets" | "cores" | "threads"))
            {
                bail!("unsupported CPU topology attribute {:?}", attribute.name());
            }
            Ok(VmCpuTopology {
                sockets: required_u32_attr(topology, "sockets")?,
                cores: required_u32_attr(topology, "cores")?,
                threads: required_u32_attr(topology, "threads")?,
            })
        })
        .transpose()?;

    Ok(Some(VmCpu {
        mode,
        model,
        vcpus: topology.is_none().then_some(vcpus),
        topology,
    }))
}

fn required_u32_attr(node: Node<'_, '_>, name: &str) -> Result<u32> {
    node.attribute(name)
        .with_context(|| format!("domain XML <{}> is missing {name}", node.tag_name().name()))?
        .parse()
        .with_context(|| format!("failed to parse domain XML {name}"))
}

fn boot_order(domain: Node<'_, '_>) -> Result<Vec<String>> {
    let os = required_child(domain, "os")?;
    let boot = os
        .children()
        .filter(|child| child.has_tag_name("boot"))
        .map(|boot| {
            let dev = boot
                .attribute("dev")
                .context("domain XML <boot> is missing dev attribute")?;
            match dev {
                "hd" | "cdrom" => Ok(dev.to_string()),
                _ => bail!("unsupported boot device {dev:?}"),
            }
        })
        .collect::<Result<Vec<_>>>()?;

    if boot.is_empty() {
        bail!("domain XML is missing boot order");
    }

    Ok(boot)
}

fn disks_from_domain_xml(devices: Node<'_, '_>) -> Result<Vec<VmDisk>> {
    let has_virtio_scsi_controller = has_virtio_scsi_controller(devices);
    let disks = devices
        .children()
        .filter(|child| child.has_tag_name("disk") && child.attribute("device") == Some("disk"))
        .map(|disk| {
            let disk_type = parse_disk_type(disk.attribute("type").unwrap_or("file"))?;
            let source = optional_child(disk, "source")
                .context("domain XML disk is missing source element")?;
            let source_attr = match disk_type {
                VmDiskType::File => "file",
                VmDiskType::Block => "dev",
            };
            let path = source
                .attribute(source_attr)
                .with_context(|| format!("domain XML disk is missing source {source_attr}"))?;
            let driver = optional_child(disk, "driver");
            let format = driver
                .and_then(|driver| driver.attribute("type"))
                .map(parse_disk_format)
                .transpose()?
                .unwrap_or(match disk_type {
                    VmDiskType::File => DiskFormat::Qcow2,
                    VmDiskType::Block => DiskFormat::Raw,
                });
            let target = optional_child(disk, "target")
                .context("domain XML disk is missing target element")?;
            let target_dev = target
                .attribute("dev")
                .context("domain XML disk target is missing dev")?
                .to_string();
            let bus = parse_disk_target_bus(
                target.attribute("bus").unwrap_or("virtio"),
                has_virtio_scsi_controller,
            )?;
            let cache = driver
                .and_then(|driver| driver.attribute("cache"))
                .map(parse_disk_cache)
                .transpose()?;
            let io = driver
                .and_then(|driver| driver.attribute("io"))
                .map(parse_disk_io)
                .transpose()?;
            let discard = driver
                .and_then(|driver| driver.attribute("discard"))
                .map(parse_disk_discard)
                .transpose()?;
            let detect_zeroes = driver
                .and_then(|driver| driver.attribute("detect_zeroes"))
                .map(parse_disk_detect_zeroes)
                .transpose()?;
            let readonly = optional_child(disk, "readonly").map(|_| true);
            let serial = optional_child(disk, "serial")
                .map(|serial| {
                    let value = serial
                        .text()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .context("domain XML disk serial is empty")?;
                    Ok::<_, anyhow::Error>(VmDiskSerial::value(value))
                })
                .transpose()?
                .unwrap_or_default();
            let io_tune = disk_iotune_from_domain_xml(disk)?;

            Ok(VmDisk {
                id: Some(disk_id_from_domain_xml(disk, &target_dev)),
                disk_type,
                path: PathBuf::from(path),
                format,
                target: Some(target_dev),
                bus,
                cache,
                io,
                discard,
                detect_zeroes,
                readonly,
                serial,
                io_tune,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(disks)
}

fn cdroms_from_domain_xml(devices: Node<'_, '_>) -> Result<Vec<VmCdromEntry>> {
    devices
        .children()
        .filter(|child| child.has_tag_name("disk") && child.attribute("device") == Some("cdrom"))
        .map(|cdrom| {
            if cdrom.attribute("type").unwrap_or("file") != "file" {
                bail!("unsupported non-file CD-ROM device");
            }
            let target = required_child(cdrom, "target")?
                .attribute("dev")
                .context("domain XML CD-ROM target is missing dev")?
                .to_string();
            let media = optional_child(cdrom, "source")
                .map(|source| {
                    source
                        .attribute("file")
                        .map(PathBuf::from)
                        .context("domain XML CD-ROM source is missing file")
                })
                .transpose()?;
            let id = optional_child(cdrom, "alias")
                .and_then(|alias| alias.attribute("name"))
                .and_then(|name| name.strip_prefix("ua-qtr-cdrom-"))
                .filter(|id| is_valid_disk_id(id))
                .map(str::to_string)
                .unwrap_or_else(|| format!("cdrom-{target}"));
            Ok(VmCdromEntry::present(VmCdrom {
                id,
                media,
                target: Some(target),
            }))
        })
        .collect()
}

fn disk_id_from_domain_xml(disk: Node<'_, '_>, target: &str) -> String {
    optional_child(disk, "alias")
        .and_then(|alias| alias.attribute("name"))
        .and_then(|name| name.strip_prefix("ua-qtr-disk-"))
        .filter(|id| is_valid_disk_id(id))
        .map(str::to_string)
        .unwrap_or_else(|| format!("disk-{target}"))
}

fn has_virtio_scsi_controller(devices: Node<'_, '_>) -> bool {
    virtio_scsi_controller(devices).is_some()
}

fn virtio_scsi_controller<'a, 'input>(devices: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    devices.children().find(|child| {
        child.has_tag_name("controller")
            && child.attribute("type") == Some("scsi")
            && child.attribute("model") == Some("virtio-scsi")
    })
}

fn io_threads_from_domain_xml(
    domain: Node<'_, '_>,
    devices: Node<'_, '_>,
) -> Result<Option<VmIoThreads>> {
    let Some(node) = optional_child(domain, "iothreads") else {
        return Ok(None);
    };
    let count = node
        .text()
        .map(str::trim)
        .context("domain XML <iothreads> is empty")?
        .parse::<u16>()
        .context("failed to parse domain iothreads")?;
    if count == 0 {
        bail!("domain iothreads must be greater than 0");
    }

    let scsi_queues = virtio_scsi_controller(devices)
        .and_then(|controller| optional_child(controller, "driver"))
        .and_then(|driver| driver.attribute("queues"))
        .map(parse_disk_queues)
        .transpose()?;
    let queues = match scsi_queues {
        Some(queues) => Some(queues),
        None => disk_driver_threads_queues(devices).transpose()?,
    };

    Ok(Some(VmIoThreads {
        count,
        queues: queues.filter(|queues| *queues != count),
    }))
}

fn disk_driver_threads_queues(devices: Node<'_, '_>) -> Option<Result<u16>> {
    devices
        .children()
        .filter(|child| child.has_tag_name("disk") && child.attribute("device") == Some("disk"))
        .filter_map(|disk| optional_child(disk, "driver"))
        .find(|driver| driver.attribute("io") == Some("threads"))
        .and_then(|driver| driver.attribute("queues"))
        .map(parse_disk_queues)
}

fn parse_disk_type(value: &str) -> Result<VmDiskType> {
    match value {
        "file" => Ok(VmDiskType::File),
        "block" => Ok(VmDiskType::Block),
        _ => bail!("unsupported disk type {value:?}"),
    }
}

fn parse_disk_target_bus(value: &str, has_virtio_scsi_controller: bool) -> Result<VmDiskBus> {
    match value {
        "virtio" => Ok(VmDiskBus::VirtioBlk),
        "scsi" if has_virtio_scsi_controller => Ok(VmDiskBus::VirtioScsi),
        "scsi" => bail!("unsupported scsi disk without virtio-scsi controller"),
        _ => bail!("unsupported disk bus {value:?}"),
    }
}

fn parse_disk_format(value: &str) -> Result<DiskFormat> {
    match value {
        "raw" => Ok(DiskFormat::Raw),
        "qcow2" => Ok(DiskFormat::Qcow2),
        _ => bail!("unsupported disk format {value:?}"),
    }
}

fn parse_disk_cache(value: &str) -> Result<VmDiskCache> {
    match value {
        "default" => Ok(VmDiskCache::Default),
        "none" => Ok(VmDiskCache::None),
        "writethrough" => Ok(VmDiskCache::Writethrough),
        "writeback" => Ok(VmDiskCache::Writeback),
        "directsync" => Ok(VmDiskCache::Directsync),
        "unsafe" => Ok(VmDiskCache::Unsafe),
        _ => bail!("unsupported disk cache mode {value:?}"),
    }
}

fn parse_disk_io(value: &str) -> Result<VmDiskIoConfig> {
    match value {
        "threads" => Ok(VmDiskIoConfig {
            mode: VmDiskIoMode::Threads,
        }),
        "native" => Ok(VmDiskIoConfig {
            mode: VmDiskIoMode::Native,
        }),
        "io_uring" => Ok(VmDiskIoConfig {
            mode: VmDiskIoMode::IoUring,
        }),
        _ => bail!("unsupported disk io mode {value:?}"),
    }
}

fn parse_disk_discard(value: &str) -> Result<VmDiskDiscard> {
    match value {
        "ignore" => Ok(VmDiskDiscard::Ignore),
        "unmap" => Ok(VmDiskDiscard::Unmap),
        _ => bail!("unsupported disk discard mode {value:?}"),
    }
}

fn parse_disk_detect_zeroes(value: &str) -> Result<VmDiskDetectZeroes> {
    match value {
        "off" => Ok(VmDiskDetectZeroes::Off),
        "on" => Ok(VmDiskDetectZeroes::On),
        "unmap" => Ok(VmDiskDetectZeroes::Unmap),
        _ => bail!("unsupported disk detect_zeroes mode {value:?}"),
    }
}

fn disk_iotune_from_domain_xml(disk: Node<'_, '_>) -> Result<VmDiskIoTuneConfig> {
    let Some(io_tune) = optional_child(disk, "iotune") else {
        return Ok(VmDiskIoTuneConfig::Preserve);
    };
    let config = VmDiskIoTune {
        total_bytes_per_sec: optional_child_u64(io_tune, "total_bytes_sec")?,
        read_bytes_per_sec: optional_child_u64(io_tune, "read_bytes_sec")?,
        write_bytes_per_sec: optional_child_u64(io_tune, "write_bytes_sec")?,
        total_iops: optional_child_u64(io_tune, "total_iops_sec")?,
        read_iops: optional_child_u64(io_tune, "read_iops_sec")?,
        write_iops: optional_child_u64(io_tune, "write_iops_sec")?,
    };
    if disk_iotune_is_empty(&config) {
        Ok(VmDiskIoTuneConfig::Preserve)
    } else {
        Ok(VmDiskIoTuneConfig::configured(config))
    }
}

fn optional_child_u64(parent: Node<'_, '_>, name: &str) -> Result<Option<u64>> {
    optional_child(parent, name)
        .map(|child| {
            child
                .text()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .with_context(|| format!("domain XML <{name}> is empty"))?
                .parse()
                .with_context(|| format!("failed to parse domain XML <{name}>"))
        })
        .transpose()
}

fn disk_iotune_is_empty(io_tune: &VmDiskIoTune) -> bool {
    [
        io_tune.total_bytes_per_sec,
        io_tune.read_bytes_per_sec,
        io_tune.write_bytes_per_sec,
        io_tune.total_iops,
        io_tune.read_iops,
        io_tune.write_iops,
    ]
    .iter()
    .all(Option::is_none)
}

fn parse_disk_queues(value: &str) -> Result<u16> {
    let queues = value
        .parse::<u16>()
        .with_context(|| format!("failed to parse disk queues {value:?}"))?;
    if queues == 0 {
        bail!("disk queues must be greater than 0");
    }

    Ok(queues)
}

fn optional_disk_source_path(
    devices: Node<'_, '_>,
    device: &str,
    target_dev: Option<&str>,
) -> Result<Option<PathBuf>> {
    for disk in devices
        .children()
        .filter(|child| child.has_tag_name("disk") && child.attribute("device") == Some(device))
    {
        if let Some(target_dev) = target_dev
            && disk_target_dev(disk).as_deref() != Some(target_dev)
        {
            continue;
        }

        let source = optional_child(disk, "source")
            .and_then(|source| source.attribute("file"))
            .with_context(|| format!("domain XML {device} disk is missing source file"))?;
        return Ok(Some(PathBuf::from(source)));
    }

    Ok(None)
}

fn disk_target_dev(disk: Node<'_, '_>) -> Option<String> {
    optional_child(disk, "target")
        .and_then(|target| target.attribute("dev"))
        .map(str::to_string)
}

fn network_name(devices: Node<'_, '_>) -> Result<String> {
    let interface = devices
        .children()
        .find(|child| child.has_tag_name("interface") && child.attribute("type") == Some("network"))
        .context("domain XML is missing network interface")?;
    Ok(optional_child(interface, "source")
        .and_then(|source| source.attribute("network"))
        .context("domain XML network interface is missing source network")?
        .to_string())
}

fn graphics_config(devices: Node<'_, '_>) -> Result<(GraphicsMode, String, Option<u16>)> {
    let Some(graphics) = devices
        .children()
        .find(|child| child.has_tag_name("graphics"))
    else {
        return Ok((GraphicsMode::None, default_vm_vnc_listen(), None));
    };

    match graphics.attribute("type") {
        Some("vnc") => {
            let listen = graphics
                .attribute("listen")
                .map(str::to_string)
                .or_else(|| {
                    optional_child(graphics, "listen")
                        .and_then(|listen| listen.attribute("address"))
                        .map(str::to_string)
                })
                .unwrap_or_else(default_vm_vnc_listen);
            let vnc_port = match graphics.attribute("port") {
                Some("-1") | None => None,
                Some(port) => Some(port.parse().context("failed to parse VNC port")?),
            };
            Ok((GraphicsMode::Vnc, listen, vnc_port))
        }
        Some(kind) => bail!("unsupported graphics type {kind:?}"),
        None => bail!("domain XML graphics device is missing type"),
    }
}

fn serial_log_path(devices: Node<'_, '_>) -> Option<PathBuf> {
    devices
        .children()
        .find(|child| child.has_tag_name("console") && child.attribute("type") == Some("file"))
        .and_then(|console| optional_child(console, "source"))
        .and_then(|source| source.attribute("path"))
        .map(PathBuf::from)
}

fn print_apply_diff(
    current_xml: &str,
    name: &str,
    manifest_path: &Path,
    desired_xml: &str,
    color: bool,
) {
    if current_xml == desired_xml {
        println!("[qtr] no changes");
        return;
    }

    let current_header = if current_xml.is_empty() {
        "/dev/null".to_string()
    } else {
        format!("current/libvirt/{name}")
    };
    let desired_path = manifest_path.strip_prefix("/").unwrap_or(manifest_path);
    let desired_header = format!("desired/{}", desired_path.display());
    let diff = TextDiff::from_lines(current_xml, desired_xml);
    let diff = diff
        .unified_diff()
        .context_radius(3)
        .header(&current_header, &desired_header)
        .to_string();

    if color {
        print!("{}", colorize_unified_diff(&diff));
    } else {
        print!("{diff}");
    }
}

fn should_color(mode: ColorMode) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Auto => io::stdout().is_terminal(),
        ColorMode::Never => false,
    }
}

fn colorize_unified_diff(diff: &str) -> String {
    diff.split_inclusive('\n')
        .map(colorize_diff_line)
        .collect::<String>()
}

fn colorize_diff_line(line: &str) -> String {
    let (content, newline) = line
        .strip_suffix('\n')
        .map_or((line, ""), |line| (line, "\n"));
    let color = if content.starts_with("--- ") || content.starts_with('-') {
        Some("\x1b[31m")
    } else if content.starts_with("+++ ") || content.starts_with('+') {
        Some("\x1b[32m")
    } else if content.starts_with("@@") {
        Some("\x1b[36m")
    } else {
        None
    };

    match color {
        Some(color) => format!("{color}{content}\x1b[0m{newline}"),
        None => line.to_string(),
    }
}

fn current_domain_xml(connect_uri: &str, name: &str) -> Result<String> {
    let conn = connect_read_only(connect_uri)?;
    let domain = match Domain::lookup_by_name(&conn, name) {
        Ok(domain) => domain,
        Err(err) if err.code() == virt::error::ErrorNumber::NoDomain => {
            return Ok(String::new());
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to look up domain {name}"));
        }
    };

    domain
        .get_xml_desc(sys::VIR_DOMAIN_XML_INACTIVE)
        .with_context(|| format!("failed to query inactive domain XML for {name}"))
}

fn existing_domain_xml(connect_uri: &str, name: &str) -> Result<String> {
    let conn = connect_read_only(connect_uri)?;
    let domain = lookup_domain(&conn, name)?;
    domain
        .get_xml_desc(sys::VIR_DOMAIN_XML_INACTIVE)
        .with_context(|| format!("failed to query inactive domain XML for {name}"))
}

fn normalize_manifest_paths(manifest: &mut VmManifest, base_dir: &Path) -> Result<()> {
    for entry in &mut manifest.disks {
        if let Some(disk) = entry.as_present_mut()
            && disk.disk_type == VmDiskType::File
        {
            disk.path = manifest_relative_path(base_dir, &disk.path);
        }
    }

    if let Some(cdrom) = &manifest.cdrom {
        manifest.cdrom = Some(manifest_relative_path(base_dir, cdrom));
    }
    if let Some(cdroms) = &mut manifest.cdroms {
        for entry in cdroms {
            if let Some(cdrom) = entry.as_present_mut()
                && let Some(media) = &cdrom.media
            {
                cdrom.media = Some(manifest_relative_path(base_dir, media));
            }
        }
    }

    if let Some(serial_log) = &manifest.serial_log {
        manifest.serial_log = Some(manifest_relative_path(base_dir, serial_log));
    }

    Ok(())
}

fn manifest_relative_path(base_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn validate_manifest(manifest: &VmManifest) -> Result<()> {
    if manifest.disks.is_empty() {
        bail!("VM definition must contain at least one disk");
    }

    if let Some(machine) = &manifest.machine
        && machine.machine_type.trim().is_empty()
    {
        bail!("machine.type must not be empty");
    }

    let memory = effective_memory(manifest)?;
    if memory.size_mib == 0 {
        bail!("memory.sizeMiB must be greater than 0");
    }
    if let Some(max_mib) = memory.max_mib
        && max_mib < memory.size_mib
    {
        bail!("memory.maxMiB must be greater than or equal to memory.sizeMiB");
    }

    let vcpus = effective_vcpus(manifest)?;
    if vcpus == 0 {
        bail!("VM vCPU count must be greater than 0");
    }
    if let Some(cpu) = &manifest.cpu {
        match cpu.mode {
            VmCpuMode::Custom => {
                if cpu
                    .model
                    .as_deref()
                    .is_none_or(|model| model.trim().is_empty())
                {
                    bail!("custom CPU mode requires cpu.model");
                }
            }
            VmCpuMode::HostPassthrough | VmCpuMode::HostModel if cpu.model.is_some() => {
                bail!("cpu.model is only valid with custom CPU mode");
            }
            _ => {}
        }
        if let Some(topology) = cpu.topology
            && (topology.sockets == 0 || topology.cores == 0 || topology.threads == 0)
        {
            bail!("CPU topology values must be greater than 0");
        }
    }

    if let Some(io_threads) = manifest.io_threads {
        if io_threads.count == 0 {
            bail!("ioThreads.count must be greater than 0");
        }
        if io_threads.queues == Some(0) {
            bail!("ioThreads.queues must be greater than 0");
        }
    }

    let uses_io_threads = manifest
        .disks
        .iter()
        .filter_map(VmDiskEntry::as_present)
        .any(|disk| disk.io.map(|io| io.mode) == Some(VmDiskIoMode::Threads));
    if uses_io_threads && manifest.io_threads.is_none() {
        bail!("io.mode threads requires ioThreads");
    }
    if !uses_io_threads && manifest.io_threads.is_some() {
        bail!("ioThreads requires at least one disk with io.mode threads");
    }

    let mut disk_ids = BTreeSet::new();
    for disk in &manifest.disks {
        if let Some(id) = disk.id() {
            validate_disk_id(id)?;
            if !disk_ids.insert(id) {
                bail!("duplicate disk id {id}");
            }
        }
    }

    let targets = assign_manifest_disk_targets(manifest)?;
    for (disk, target) in present_disks(&manifest.disks).zip(&targets) {
        validate_disk_target_for_bus(target, disk.bus)?;
        if disk
            .serial
            .as_deref()
            .is_some_and(|serial| serial.trim().is_empty())
        {
            bail!("disk serial must not be empty");
        }
        if let Some(io_tune) = disk.io_tune.as_ref() {
            validate_disk_iotune(io_tune)?;
        }
        if !disk.path.exists() {
            bail!("disk {} does not exist", disk.path.display());
        }
        match disk.disk_type {
            VmDiskType::File => {
                if !disk.path.is_file() {
                    bail!("file disk {} is not a regular file", disk.path.display());
                }
            }
            VmDiskType::Block => {
                if !disk.path.is_absolute() {
                    bail!(
                        "block disk {} must be an absolute path",
                        disk.path.display()
                    );
                }
                let metadata = fs::metadata(&disk.path).with_context(|| {
                    format!("failed to inspect block disk {}", disk.path.display())
                })?;
                if !metadata.file_type().is_block_device() {
                    bail!("block disk {} is not a block device", disk.path.display());
                }
                if disk.format != DiskFormat::Raw {
                    bail!("block disk {} must use format raw", disk.path.display());
                }
            }
        }
    }

    if manifest.cdrom.is_some() && manifest.cdroms.is_some() {
        bail!("cdrom and cdroms are mutually exclusive");
    }
    if let Some(cdroms) = &manifest.cdroms {
        let mut ids = BTreeSet::new();
        for entry in cdroms {
            validate_cdrom_id(entry.id())?;
            if !ids.insert(entry.id()) {
                bail!("duplicate CD-ROM id {}", entry.id());
            }
            if let Some(cdrom) = entry.as_present()
                && let Some(media) = &cdrom.media
                && !media.is_file()
            {
                bail!("CD-ROM media {} does not exist", media.display());
            }
        }
        assign_manifest_cdrom_targets(manifest)?;
    }

    if let Some(cdrom) = &manifest.cdrom
        && !cdrom.exists()
    {
        bail!("cdrom ISO {} does not exist", cdrom.display());
    }

    Ok(())
}

fn validate_disk_iotune(io_tune: &VmDiskIoTune) -> Result<()> {
    if disk_iotune_is_empty(io_tune) {
        bail!("disk ioTune must contain at least one limit");
    }
    if io_tune.total_bytes_per_sec.unwrap_or(0) > 0
        && (io_tune.read_bytes_per_sec.unwrap_or(0) > 0
            || io_tune.write_bytes_per_sec.unwrap_or(0) > 0)
    {
        bail!(
            "ioTune.totalBytesPerSec cannot be combined with readBytesPerSec or writeBytesPerSec"
        );
    }
    if io_tune.total_iops.unwrap_or(0) > 0
        && (io_tune.read_iops.unwrap_or(0) > 0 || io_tune.write_iops.unwrap_or(0) > 0)
    {
        bail!("ioTune.totalIops cannot be combined with readIops or writeIops");
    }
    Ok(())
}

fn validate_disk_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 48 {
        bail!("disk id must contain 1 to 48 characters");
    }
    if !is_valid_disk_id(id) {
        bail!("disk id {id:?} contains unsupported characters");
    }
    Ok(())
}

fn validate_cdrom_id(id: &str) -> Result<()> {
    if !is_valid_disk_id(id) {
        bail!("CD-ROM id {id:?} must contain 1 to 48 letters, numbers, '-', '_' or '.'");
    }
    Ok(())
}

fn validate_cdrom_target(target: &str) -> Result<()> {
    if !target.starts_with("sd") || target.len() <= 2 {
        bail!("CD-ROM target {target} must start with sd");
    }
    Ok(())
}

fn is_valid_disk_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn manifest_boot_order(manifest: &VmManifest) -> String {
    match &manifest.boot {
        Some(boot) => boot.join(","),
        None if manifest.cdrom.is_some() => "cdrom,hd".to_string(),
        None => "hd".to_string(),
    }
}

fn list(args: VmListArgs) -> Result<()> {
    let conn = connect_read_only(&args.connect_uri)?;
    let flags = sys::VIR_CONNECT_LIST_DOMAINS_ACTIVE | sys::VIR_CONNECT_LIST_DOMAINS_INACTIVE;
    let mut rows = conn
        .list_all_domains(flags)
        .context("failed to list domains")?
        .into_iter()
        .map(|domain| domain_list_row(&domain))
        .collect::<Result<Vec<_>>>()?;

    rows.sort_by(|left, right| left.name.cmp(&right.name));

    crate::cli_table::print_table(
        &["NAME", "STATE", "ID"],
        rows.into_iter()
            .map(|row| vec![row.name, row.state.to_string(), row.id]),
    );

    Ok(())
}

pub fn list_summaries(connect_uri: &str) -> Result<Vec<VmSummary>> {
    let conn = connect_read_only(connect_uri)?;
    let flags = sys::VIR_CONNECT_LIST_DOMAINS_ACTIVE | sys::VIR_CONNECT_LIST_DOMAINS_INACTIVE;
    let mut summaries = conn
        .list_all_domains(flags)
        .context("failed to list domains")?
        .into_iter()
        .map(|domain| domain_summary(&domain))
        .collect::<Result<Vec<_>>>()?;

    summaries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(summaries)
}

pub fn get_summary(connect_uri: &str, name: &str) -> VmApiResult<VmSummary> {
    let conn = connect_read_only(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    domain_summary(&domain).map_err(VmApiError::Internal)
}

fn domain_summary(domain: &Domain) -> Result<VmSummary> {
    let name = domain.get_name().context("failed to query domain name")?;
    let (state, _) = domain
        .get_state()
        .with_context(|| format!("failed to query domain {name} state"))?;
    let id = domain.get_id().map(|id| id.to_string());
    let xml = domain
        .get_xml_desc(0)
        .with_context(|| format!("failed to query domain {name} XML"))?;
    let vnc = xml.contains("<graphics type='vnc'") || xml.contains("<graphics type=\"vnc\"");
    let vnc_endpoint = parse_vnc_endpoint(&xml, "127.0.0.1").map(|endpoint| endpoint.display());
    let serial_log = parse_serial_log(&xml);
    let (memory_mib, vcpus, network) = parse_summary_resources(&xml);
    let (disks, io_threads, cdrom, boot, graphics, vnc_listen, vnc_port) =
        parse_summary_definition(&xml).unwrap_or_default();
    let metrics = domain_metrics(domain, &xml);

    Ok(VmSummary {
        name,
        state: domain_state_name(state),
        id,
        vnc,
        vnc_endpoint,
        serial_log,
        memory_mib,
        vcpus,
        io_threads,
        network,
        disks,
        cdrom,
        boot,
        graphics,
        vnc_listen,
        vnc_port,
        metrics,
    })
}

fn domain_metrics(domain: &Domain, xml: &str) -> Option<VmMetrics> {
    if !domain.is_active().ok()? {
        return None;
    }

    let info = domain.get_info().ok()?;
    let (rx_bytes, tx_bytes) = interface_byte_totals(domain, xml);
    Some(VmMetrics {
        cpu_time_ns: info.cpu_time,
        memory_used_mib: kib_to_mib(domain_memory_used_kib(domain, &info)),
        memory_total_mib: kib_to_mib(info.max_mem.max(info.memory)),
        tx_bytes,
        rx_bytes,
        sampled_at_ms: sampled_at_ms(),
    })
}

fn domain_memory_used_kib(domain: &Domain, info: &DomainInfo) -> u64 {
    if let Ok(stats) = domain.memory_stats(0) {
        let actual = memory_stat_value(&stats, sys::VIR_DOMAIN_MEMORY_STAT_ACTUAL_BALLOON);
        let unused = memory_stat_value(&stats, sys::VIR_DOMAIN_MEMORY_STAT_UNUSED);
        if let Some(actual) = actual {
            return actual.saturating_sub(unused.unwrap_or(0));
        }

        if let Some(rss) = memory_stat_value(&stats, sys::VIR_DOMAIN_MEMORY_STAT_RSS) {
            return rss;
        }
    }

    info.memory
}

fn memory_stat_value(stats: &[MemoryStat], tag: u32) -> Option<u64> {
    stats
        .iter()
        .find(|stat| stat.tag == tag)
        .map(|stat| stat.val)
}

fn interface_byte_totals(domain: &Domain, xml: &str) -> (u64, u64) {
    interface_targets(xml)
        .into_iter()
        .filter_map(|target| domain.interface_stats(&target).ok())
        .fold((0, 0), |(rx_total, tx_total), stats| {
            (
                rx_total + non_negative_stat(stats.rx_bytes),
                tx_total + non_negative_stat(stats.tx_bytes),
            )
        })
}

fn interface_targets(xml: &str) -> Vec<String> {
    let Ok(doc) = Document::parse(xml) else {
        return Vec::new();
    };
    let Ok(devices) = required_child(doc.root_element(), "devices") else {
        return Vec::new();
    };

    devices
        .children()
        .filter(|child| child.has_tag_name("interface"))
        .filter_map(|interface| optional_child(interface, "target"))
        .filter_map(|target| target.attribute("dev"))
        .map(str::to_string)
        .collect()
}

fn non_negative_stat(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn kib_to_mib(value: u64) -> u64 {
    value / 1024
}

fn sampled_at_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn parse_summary_resources(xml: &str) -> (Option<u64>, Option<u32>, Option<String>) {
    let Ok(doc) = Document::parse(xml) else {
        return (None, None, None);
    };
    let domain = doc.root_element();
    let memory = memory_mib(domain).ok();
    let vcpus = required_child_text(domain, "vcpu")
        .ok()
        .and_then(|value| value.parse().ok());
    let network = required_child(domain, "devices")
        .ok()
        .and_then(|devices| network_name(devices).ok());

    (memory, vcpus, network)
}

#[allow(clippy::type_complexity)]
fn parse_summary_definition(
    xml: &str,
) -> Result<(
    Option<Vec<VmSummaryDisk>>,
    Option<VmIoThreads>,
    Option<String>,
    Option<Vec<String>>,
    String,
    Option<String>,
    Option<u16>,
)> {
    let doc = Document::parse(xml).context("failed to parse domain XML")?;
    let domain = doc.root_element();
    let devices = required_child(domain, "devices")?;

    let disks = disks_from_domain_xml(devices).ok().map(|disks| {
        disks
            .into_iter()
            .enumerate()
            .map(|(index, disk)| VmSummaryDisk {
                disk_type: disk.disk_type,
                path: disk.path.display().to_string(),
                format: disk.format,
                target: disk_target_or(&disk, index),
                bus: disk.bus,
                cache: disk.cache,
                io: disk.io,
            })
            .collect()
    });
    let io_threads = io_threads_from_domain_xml(domain, devices).ok().flatten();
    let cdrom = optional_disk_source_path(devices, "cdrom", None)
        .ok()
        .flatten()
        .map(|path| path.display().to_string());
    let boot = boot_order(domain).ok();
    let (graphics, vnc_listen, vnc_port) = graphics_config(devices)?;
    let graphics = match graphics {
        GraphicsMode::None => "none",
        GraphicsMode::Vnc => "vnc",
    };

    Ok((
        disks,
        io_threads,
        cdrom,
        boot,
        graphics.to_string(),
        Some(vnc_listen),
        vnc_port,
    ))
}

struct DomainListRow {
    name: String,
    state: &'static str,
    id: String,
}

fn domain_list_row(domain: &Domain) -> Result<DomainListRow> {
    let name = domain.get_name().context("failed to query domain name")?;
    let (state, _) = domain
        .get_state()
        .with_context(|| format!("failed to query domain {name} state"))?;
    let id = domain
        .get_id()
        .map(|id| id.to_string())
        .unwrap_or_else(|| "-".to_string());

    Ok(DomainListRow {
        name,
        state: domain_state_name(state),
        id,
    })
}

fn domain_state_name(state: sys::virDomainState) -> &'static str {
    match state {
        sys::VIR_DOMAIN_NOSTATE => "nostate",
        sys::VIR_DOMAIN_RUNNING => "running",
        sys::VIR_DOMAIN_BLOCKED => "blocked",
        sys::VIR_DOMAIN_PAUSED => "paused",
        sys::VIR_DOMAIN_SHUTDOWN => "shutdown",
        sys::VIR_DOMAIN_SHUTOFF => "shutoff",
        sys::VIR_DOMAIN_CRASHED => "crashed",
        sys::VIR_DOMAIN_PMSUSPENDED => "pmsuspended",
        _ => "unknown",
    }
}

fn start(args: VmStartArgs) -> Result<()> {
    if args.rm_after_shutdown && !args.wait_shutdown {
        bail!("--rm-after-shutdown requires --wait-shutdown");
    }

    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    start_domain(&domain, &args.name)?;

    print_vnc_endpoint(&domain, "127.0.0.1")?;
    print_serial_log(&domain)?;

    if args.wait_shutdown {
        eprintln!("[qtr] waiting for guest shutdown...");
        wait_shutdown_domain(
            &domain,
            &args.name,
            args.shutdown_timeout_secs.map(Duration::from_secs),
        )?;
        if args.rm_after_shutdown {
            undefine_domain(&domain, &args.name)?;
        }
    }

    Ok(())
}

fn stop(args: VmStopArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {} state", args.name))?
    {
        eprintln!("[qtr] VM already stopped: {}", args.name);
        return Ok(());
    }

    if args.force {
        domain
            .destroy()
            .with_context(|| format!("failed to destroy domain {}", args.name))?;
        eprintln!("[qtr] force stopped VM: {}", args.name);
    } else {
        domain
            .shutdown()
            .with_context(|| format!("failed to request shutdown for domain {}", args.name))?;
        eprintln!("[qtr] shutdown requested: {}", args.name);
    }

    if args.wait {
        wait_shutdown_domain(
            &domain,
            &args.name,
            args.shutdown_timeout_secs.map(Duration::from_secs),
        )?;
    }

    Ok(())
}

fn reboot(args: VmNameArgs) -> Result<()> {
    reboot_by_name(&args.connect_uri, &args.name).map_err(anyhow::Error::new)?;
    eprintln!("[qtr] reboot requested: {}", args.name);
    Ok(())
}

fn reset(args: VmNameArgs) -> Result<()> {
    reset_by_name(&args.connect_uri, &args.name).map_err(anyhow::Error::new)?;
    eprintln!("[qtr] reset VM: {}", args.name);
    Ok(())
}

fn suspend(args: VmNameArgs) -> Result<()> {
    let changed = suspend_by_name(&args.connect_uri, &args.name).map_err(anyhow::Error::new)?;
    if changed {
        eprintln!("[qtr] suspended VM: {}", args.name);
    } else {
        eprintln!("[qtr] VM already suspended: {}", args.name);
    }
    Ok(())
}

fn resume(args: VmNameArgs) -> Result<()> {
    let changed = resume_by_name(&args.connect_uri, &args.name).map_err(anyhow::Error::new)?;
    if changed {
        eprintln!("[qtr] resumed VM: {}", args.name);
    } else {
        eprintln!("[qtr] VM already running: {}", args.name);
    }
    Ok(())
}

fn autostart(args: VmAutostartArgs) -> Result<()> {
    let desired = if args.enable {
        Some(true)
    } else if args.disable {
        Some(false)
    } else {
        None
    };
    let enabled =
        autostart_by_name(&args.connect_uri, &args.name, desired).map_err(anyhow::Error::new)?;
    println!("{}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}

fn managed_save(args: VmNameArgs) -> Result<()> {
    managed_save_by_name(&args.connect_uri, &args.name).map_err(anyhow::Error::new)?;
    eprintln!("[qtr] saved VM state: {}", args.name);
    Ok(())
}

fn restore_managed_save(args: VmNameArgs) -> Result<()> {
    restore_managed_save_by_name(&args.connect_uri, &args.name).map_err(anyhow::Error::new)?;
    eprintln!("[qtr] restored VM state: {}", args.name);
    Ok(())
}

fn managed_save_state(args: VmSavedStateArgs) -> Result<()> {
    if args.remove {
        let removed = remove_managed_save_by_name(&args.connect_uri, &args.name)
            .map_err(anyhow::Error::new)?;
        if removed {
            eprintln!("[qtr] removed saved VM state: {}", args.name);
        } else {
            eprintln!("[qtr] VM has no saved state: {}", args.name);
        }
    } else {
        let present =
            has_managed_save_by_name(&args.connect_uri, &args.name).map_err(anyhow::Error::new)?;
        println!("{}", if present { "present" } else { "absent" });
    }
    Ok(())
}

fn remove(args: VmRemoveArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    if domain
        .is_active()
        .with_context(|| format!("failed to query domain {} state", args.name))?
    {
        if !args.force_stop {
            bail!(
                "domain {} is active; stop it first or pass --force-stop",
                args.name
            );
        }

        domain
            .destroy()
            .with_context(|| format!("failed to destroy domain {}", args.name))?;
        eprintln!("[qtr] force stopped VM: {}", args.name);
    }

    undefine_domain(&domain, &args.name)
}

fn vnc(args: VmNameArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {} state", args.name))?
    {
        bail!("domain {} is not active", args.name);
    }

    let endpoint = query_vnc_endpoint(&domain, "127.0.0.1")?.with_context(|| {
        format!(
            "domain {} does not expose an active VNC endpoint",
            args.name
        )
    })?;
    println!("{endpoint}");

    Ok(())
}

fn cp(args: VmCpArgs) -> Result<()> {
    let source = parse_copy_endpoint(&args.source)?;
    let dest = parse_copy_endpoint(&args.dest)?;

    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {} state", args.name))?
    {
        bail!("domain {} is not active", args.name);
    }

    let timeout = Duration::from_secs(args.timeout_secs);
    let deadline = guest_agent::GuestAgentDeadline::new(timeout);
    guest_agent::wait_ready_with_deadline(&domain, &deadline)
        .with_context(|| format!("guest agent is not ready for domain {}", args.name))?;

    match (source, dest) {
        (VmCopyEndpoint::Host(source), VmCopyEndpoint::Guest(dest)) => {
            if args.parents {
                create_guest_parent_dir(&domain, &dest, &deadline)?;
            }
            let contents = fs::read(&source)
                .with_context(|| format!("failed to read {}", source.display()))?;
            guest_agent::write_file_with_deadline(&domain, &dest, &contents, &deadline)
                .with_context(|| format!("failed to write guest file {dest}"))?;
            eprintln!("[qtr] copied {} to guest:{dest}", source.display());
        }
        (VmCopyEndpoint::Guest(source), VmCopyEndpoint::Host(dest)) => {
            if args.parents {
                create_host_parent_dir(&dest)?;
            }
            let contents = guest_agent::read_file_with_deadline(&domain, &source, &deadline)
                .with_context(|| format!("failed to read guest file {source}"))?;
            fs::write(&dest, contents)
                .with_context(|| format!("failed to write {}", dest.display()))?;
            eprintln!("[qtr] copied guest:{source} to {}", dest.display());
        }
        (VmCopyEndpoint::Guest(_), VmCopyEndpoint::Guest(_)) => {
            bail!("guest-to-guest copy is not supported")
        }
        (VmCopyEndpoint::Host(_), VmCopyEndpoint::Host(_)) => {
            bail!("one of SRC or DEST must be prefixed with guest:")
        }
    }

    Ok(())
}

fn parse_copy_endpoint(value: &str) -> Result<VmCopyEndpoint> {
    match value.strip_prefix("guest:") {
        Some(path) if !path.is_empty() => Ok(VmCopyEndpoint::Guest(path.to_string())),
        Some(_) => bail!("guest path must not be empty"),
        None => Ok(VmCopyEndpoint::Host(PathBuf::from(value))),
    }
}

fn create_host_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    Ok(())
}

fn create_guest_parent_dir(
    domain: &Domain,
    path: &str,
    deadline: &guest_agent::GuestAgentDeadline,
) -> Result<()> {
    let Some(parent) = Path::new(path)
        .parent()
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty())
    else {
        return Ok(());
    };

    let command = format!("mkdir -p {}", shell_quote(&parent));
    let result = guest_agent::run_command_with_deadline(domain, &command, deadline)
        .with_context(|| format!("failed to create guest directory {parent}"))?;
    if result.exitcode != 0 {
        bail!(
            "failed to create guest directory {}: {}",
            parent,
            String::from_utf8_lossy(&result.stderr)
        );
    }

    Ok(())
}

fn exec(args: VmExecArgs) -> Result<()> {
    if args.script.is_some() && !args.command.is_empty() {
        bail!("--script and command arguments are mutually exclusive");
    }
    if args.script.is_none() && args.command.is_empty() {
        bail!("provide either --script FILE or a command after --");
    }

    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {} state", args.name))?
    {
        bail!("domain {} is not active", args.name);
    }

    let timeout = Duration::from_secs(args.timeout_secs);
    let deadline = guest_agent::GuestAgentDeadline::new(timeout);
    guest_agent::wait_ready_with_deadline(&domain, &deadline)
        .with_context(|| format!("guest agent is not ready for domain {}", args.name))?;

    let (mode, command, script, guest_path) = match &args.script {
        Some(script) => {
            let contents = fs::read(script)
                .with_context(|| format!("failed to read script {}", script.display()))?;
            let guest_path = format!("/tmp/qtr-exec-{}.sh", Uuid::new_v4());
            guest_agent::write_file_with_deadline(&domain, &guest_path, &contents, &deadline)
                .with_context(|| format!("failed to upload script to guest {guest_path}"))?;
            (
                VmExecMode::Script,
                format!("/bin/sh {}", shell_quote(&guest_path)),
                Some(script.display().to_string()),
                Some(guest_path),
            )
        }
        None => (
            VmExecMode::Command,
            join_command_args(&args.command),
            None,
            None,
        ),
    };

    let started = Instant::now();
    let exec_result = if args.output.is_some() {
        guest_agent::run_command_with_deadline(&domain, &command, &deadline)
    } else {
        stream_guest_command(&domain, &command, &deadline)
    };
    let elapsed_ms = started.elapsed().as_millis();
    if let Some(guest_path) = &guest_path {
        let cleanup = format!("rm -f {}", shell_quote(guest_path));
        if let Err(err) = guest_agent::run_command(&domain, &cleanup, Duration::from_secs(30)) {
            eprintln!("[qtr] warning: failed to remove guest script {guest_path}: {err}");
        }
    }

    let result = exec_result
        .with_context(|| format!("failed to run guest command in domain {}", args.name))?;
    if result.stdout_truncated || result.stderr_truncated {
        eprintln!("[qtr] warning: qemu guest agent truncated captured command output");
    }

    if let Some(output_path) = &args.output {
        let output = VmExecOutput {
            domain: args.name.clone(),
            mode,
            command,
            script,
            guest_path,
            exit_code: result.exitcode,
            elapsed_ms,
            stdout: String::from_utf8_lossy(&result.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
            stdout_truncated: result.stdout_truncated,
            stderr_truncated: result.stderr_truncated,
        };
        write_exec_output(output_path, &output)?;
    } else {
        io::stdout()
            .write_all(&result.stdout)
            .context("failed to write guest stdout")?;
        io::stderr()
            .write_all(&result.stderr)
            .context("failed to write guest stderr")?;
    }

    if result.exitcode != 0 {
        bail!("guest command exited with {}", result.exitcode);
    }

    Ok(())
}

fn stream_guest_command(
    domain: &Domain,
    command: &str,
    deadline: &guest_agent::GuestAgentDeadline,
) -> Result<guest_agent::GuestExecResult> {
    const STREAM_CHUNK_SIZE: i64 = 48 * 1024;

    let id = Uuid::new_v4();
    let stdout_path = format!("/tmp/qtr-exec-{id}.stdout");
    let stderr_path = format!("/tmp/qtr-exec-{id}.stderr");
    let mut stdout_stream = GuestOutputStream {
        path: stdout_path.clone(),
        offset: 0,
    };
    let mut stderr_stream = GuestOutputStream {
        path: stderr_path.clone(),
        offset: 0,
    };

    guest_agent::write_file_with_deadline(domain, &stdout_path, b"", deadline)
        .with_context(|| format!("failed to create guest stdout file {stdout_path}"))?;
    guest_agent::write_file_with_deadline(domain, &stderr_path, b"", deadline)
        .with_context(|| format!("failed to create guest stderr file {stderr_path}"))?;

    let run_result = (|| {
        let wrapped_command = format!(
            "( {} ) > {} 2> {}",
            command,
            shell_quote(&stdout_path),
            shell_quote(&stderr_path)
        );
        let child = guest_agent::start_command(domain, &wrapped_command, false, deadline)?;

        loop {
            if deadline.expired() {
                terminate_guest_command(domain, child.pid);
                bail!("timed out waiting for guest command pid {}", child.pid);
            }

            drain_guest_output_stream(
                domain,
                &mut stdout_stream,
                &mut io::stdout(),
                "stdout",
                STREAM_CHUNK_SIZE,
                deadline,
            )?;
            drain_guest_output_stream(
                domain,
                &mut stderr_stream,
                &mut io::stderr(),
                "stderr",
                STREAM_CHUNK_SIZE,
                deadline,
            )?;

            let status = guest_agent::query_exec_status(domain, child.pid, deadline)?;
            if status.exited {
                drain_guest_output_stream(
                    domain,
                    &mut stdout_stream,
                    &mut io::stdout(),
                    "stdout",
                    STREAM_CHUNK_SIZE,
                    deadline,
                )?;
                drain_guest_output_stream(
                    domain,
                    &mut stderr_stream,
                    &mut io::stderr(),
                    "stderr",
                    STREAM_CHUNK_SIZE,
                    deadline,
                )?;

                return Ok(guest_agent::GuestExecResult {
                    exitcode: status
                        .exitcode
                        .context("guest command exited without exit code")?,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                });
            }

            let Some(remaining) = deadline.remaining() else {
                terminate_guest_command(domain, child.pid);
                bail!("timed out waiting for guest command pid {}", child.pid);
            };

            thread::sleep(remaining.min(Duration::from_secs(1)));
        }
    })();

    cleanup_guest_paths(domain, &[&stdout_path, &stderr_path]);

    run_result
}

fn drain_guest_output_stream<W: Write>(
    domain: &Domain,
    stream: &mut GuestOutputStream,
    writer: &mut W,
    stream_name: &str,
    chunk_size: i64,
    deadline: &guest_agent::GuestAgentDeadline,
) -> Result<()> {
    loop {
        let chunk =
            guest_agent::read_file_from(domain, &stream.path, stream.offset, chunk_size, deadline)
                .with_context(|| {
                    format!("failed to read guest {stream_name} file {}", stream.path)
                })?;
        if chunk.data.is_empty() {
            return Ok(());
        }

        writer
            .write_all(&chunk.data)
            .with_context(|| format!("failed to write guest {stream_name}"))?;
        writer
            .flush()
            .with_context(|| format!("failed to flush guest {stream_name}"))?;
        stream.offset += chunk.data.len() as i64;

        if chunk.eof {
            return Ok(());
        }
    }
}

fn terminate_guest_command(domain: &Domain, pid: i64) {
    if let Err(err) = guest_agent::terminate_command(domain, pid) {
        eprintln!("[qtr] warning: failed to terminate guest process {pid}: {err}");
    }
}

fn cleanup_guest_paths(domain: &Domain, paths: &[&str]) {
    if paths.is_empty() {
        return;
    }

    let quoted_paths = paths
        .iter()
        .map(|path| shell_quote(path))
        .collect::<Vec<_>>()
        .join(" ");
    let cleanup = format!("rm -f {quoted_paths}");
    if let Err(err) = guest_agent::run_command(domain, &cleanup, Duration::from_secs(30)) {
        eprintln!("[qtr] warning: failed to remove guest output files: {err}");
    }
}

fn write_exec_output(path: &Path, output: &VmExecOutput) -> Result<()> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(output).context("failed to serialize exec output")?;
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn join_command_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn reboot_by_name(connect_uri: &str, name: &str) -> VmApiResult<()> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    ensure_active_domain(&domain, name, "reboot")?;
    domain
        .reboot(0)
        .with_context(|| format!("failed to reboot domain {name}"))
        .map_err(VmApiError::Internal)
}

pub fn reset_by_name(connect_uri: &str, name: &str) -> VmApiResult<()> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    ensure_active_domain(&domain, name, "reset")?;
    domain
        .reset()
        .with_context(|| format!("failed to reset domain {name}"))
        .map(|_| ())
        .map_err(VmApiError::Internal)
}

pub fn suspend_by_name(connect_uri: &str, name: &str) -> VmApiResult<bool> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    let state = domain
        .get_state()
        .with_context(|| format!("failed to query domain {name} state"))
        .map_err(VmApiError::Internal)?
        .0;
    match suspend_state_action(state) {
        Some(false) => Ok(false),
        Some(true) => domain
            .suspend()
            .with_context(|| format!("failed to suspend domain {name}"))
            .map(|_| true)
            .map_err(VmApiError::Internal),
        None => Err(VmApiError::Conflict(anyhow::anyhow!(
            "domain {name} cannot be suspended from state {}",
            domain_state_name(state)
        ))),
    }
}

pub fn resume_by_name(connect_uri: &str, name: &str) -> VmApiResult<bool> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    let state = domain
        .get_state()
        .with_context(|| format!("failed to query domain {name} state"))
        .map_err(VmApiError::Internal)?
        .0;
    match resume_state_action(state) {
        Some(false) => Ok(false),
        Some(true) => domain
            .resume()
            .with_context(|| format!("failed to resume domain {name}"))
            .map(|_| true)
            .map_err(VmApiError::Internal),
        None => Err(VmApiError::Conflict(anyhow::anyhow!(
            "domain {name} cannot be resumed from state {}",
            domain_state_name(state)
        ))),
    }
}

pub fn autostart_by_name(
    connect_uri: &str,
    name: &str,
    desired: Option<bool>,
) -> VmApiResult<bool> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    if let Some(desired) = desired {
        domain
            .set_autostart(desired)
            .with_context(|| format!("failed to set autostart for domain {name}"))
            .map_err(VmApiError::Internal)?;
    }
    domain
        .get_autostart()
        .with_context(|| format!("failed to query autostart for domain {name}"))
        .map_err(VmApiError::Internal)
}

pub fn managed_save_by_name(connect_uri: &str, name: &str) -> VmApiResult<()> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    ensure_active_domain(&domain, name, "save its state")?;
    domain
        .managed_save(0)
        .with_context(|| format!("failed to save state for domain {name}"))
        .map(|_| ())
        .map_err(VmApiError::Internal)
}

pub fn restore_managed_save_by_name(connect_uri: &str, name: &str) -> VmApiResult<()> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    if domain
        .is_active()
        .with_context(|| format!("failed to query domain {name} state"))
        .map_err(VmApiError::Internal)?
    {
        return Err(VmApiError::Conflict(anyhow::anyhow!(
            "domain {name} must be inactive to restore its saved state"
        )));
    }
    if !has_managed_save(&domain, name)? {
        return Err(VmApiError::Conflict(anyhow::anyhow!(
            "domain {name} has no managed save image"
        )));
    }
    domain
        .create()
        .with_context(|| format!("failed to restore saved state for domain {name}"))
        .map(|_| ())
        .map_err(VmApiError::Internal)
}

pub fn has_managed_save_by_name(connect_uri: &str, name: &str) -> VmApiResult<bool> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    has_managed_save(&domain, name)
}

pub fn remove_managed_save_by_name(connect_uri: &str, name: &str) -> VmApiResult<bool> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    if !has_managed_save(&domain, name)? {
        return Ok(false);
    }
    domain
        .managed_save_remove(0)
        .with_context(|| format!("failed to remove saved state for domain {name}"))
        .map(|_| true)
        .map_err(VmApiError::Internal)
}

fn has_managed_save(domain: &Domain, name: &str) -> VmApiResult<bool> {
    domain
        .has_managed_save(0)
        .with_context(|| format!("failed to query saved state for domain {name}"))
        .map_err(VmApiError::Internal)
}

fn ensure_active_domain(domain: &Domain, name: &str, operation: &str) -> VmApiResult<()> {
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {name} state"))
        .map_err(VmApiError::Internal)?
    {
        return Err(VmApiError::Conflict(anyhow::anyhow!(
            "domain {name} must be active to {operation}"
        )));
    }
    Ok(())
}

fn suspend_state_action(state: sys::virDomainState) -> Option<bool> {
    match state {
        sys::VIR_DOMAIN_PAUSED => Some(false),
        sys::VIR_DOMAIN_RUNNING | sys::VIR_DOMAIN_BLOCKED => Some(true),
        _ => None,
    }
}

fn resume_state_action(state: sys::virDomainState) -> Option<bool> {
    match state {
        sys::VIR_DOMAIN_RUNNING | sys::VIR_DOMAIN_BLOCKED => Some(false),
        sys::VIR_DOMAIN_PAUSED => Some(true),
        _ => None,
    }
}

pub fn start_by_name(connect_uri: &str, name: &str) -> VmApiResult<()> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    start_domain(&domain, name).map_err(VmApiError::Internal)
}

pub fn shutdown_by_name(connect_uri: &str, name: &str, wait: bool) -> VmApiResult<()> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {name} state"))
        .map_err(VmApiError::Internal)?
    {
        return Ok(());
    }

    domain
        .shutdown()
        .with_context(|| format!("failed to request shutdown for domain {name}"))
        .map_err(VmApiError::Internal)?;
    if wait {
        wait_shutdown_domain(&domain, name, None).map_err(VmApiError::Internal)?;
    }

    Ok(())
}

pub fn destroy_by_name(connect_uri: &str, name: &str) -> VmApiResult<()> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {name} state"))
        .map_err(VmApiError::Internal)?
    {
        return Ok(());
    }

    domain
        .destroy()
        .with_context(|| format!("failed to destroy domain {name}"))
        .map_err(VmApiError::Internal)
}

pub fn undefine_by_name(connect_uri: &str, name: &str) -> VmApiResult<()> {
    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = lookup_domain_api(&conn, name)?;
    if domain
        .is_active()
        .with_context(|| format!("failed to query domain {name} state"))
        .map_err(VmApiError::Internal)?
    {
        return Err(VmApiError::Conflict(anyhow::anyhow!(
            "domain {name} is active; shutdown or destroy it first"
        )));
    }

    domain
        .undefine()
        .with_context(|| format!("failed to undefine domain {name}"))
        .map_err(VmApiError::Internal)
}

pub fn create_by_manifest(connect_uri: &str, mut manifest: VmManifest) -> VmApiResult<VmSummary> {
    let base_dir = env::current_dir()
        .context("failed to determine current directory")
        .map_err(VmApiError::Internal)?;
    normalize_manifest_paths(&mut manifest, &base_dir).map_err(VmApiError::InvalidRequest)?;
    validate_manifest(&manifest).map_err(VmApiError::InvalidRequest)?;
    validate_new_vm_disks(&manifest).map_err(VmApiError::InvalidRequest)?;

    let boot = manifest_boot_order(&manifest);
    let boot_devices = parse_boot_devices(&boot).map_err(VmApiError::InvalidRequest)?;
    if boot_devices.contains(&BootDevice::Cdrom) && !manifest_has_cdrom(&manifest) {
        return Err(VmApiError::InvalidRequest(anyhow::anyhow!(
            "boot order contains cdrom but cdrom was not provided"
        )));
    }

    let xml =
        build_manifest_domain_xml(&manifest, &boot_devices).map_err(VmApiError::InvalidRequest)?;

    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    ensure_domain_absent_api(&conn, &manifest.name)?;
    prepare_serial_log_path(manifest.serial_log.as_deref()).map_err(VmApiError::Internal)?;

    let domain = Domain::define_xml_flags(&conn, &xml, sys::VIR_DOMAIN_DEFINE_VALIDATE)
        .with_context(|| format!("failed to define domain {}", manifest.name))
        .map_err(VmApiError::Internal)?;

    domain_summary(&domain).map_err(VmApiError::Internal)
}

pub fn apply_by_manifest(connect_uri: &str, mut manifest: VmManifest) -> VmApiResult<VmSummary> {
    let base_dir = env::current_dir()
        .context("failed to determine current directory")
        .map_err(VmApiError::Internal)?;
    normalize_manifest_paths(&mut manifest, &base_dir).map_err(VmApiError::InvalidRequest)?;
    validate_manifest(&manifest).map_err(VmApiError::InvalidRequest)?;

    let boot = manifest_boot_order(&manifest);
    let boot_devices = parse_boot_devices(&boot).map_err(VmApiError::InvalidRequest)?;
    if boot_devices.contains(&BootDevice::Cdrom) && !manifest_has_cdrom(&manifest) {
        return Err(VmApiError::InvalidRequest(anyhow::anyhow!(
            "boot order contains cdrom but cdrom was not provided"
        )));
    }

    let current_xml =
        current_domain_xml(connect_uri, &manifest.name).map_err(VmApiError::Internal)?;
    let xml = if current_xml.is_empty() {
        validate_new_vm_disks(&manifest).map_err(VmApiError::InvalidRequest)?;
        build_manifest_domain_xml(&manifest, &boot_devices).map_err(VmApiError::InvalidRequest)?
    } else {
        patch_domain_xml(&current_xml, &manifest, &boot_devices)
            .map_err(VmApiError::InvalidRequest)?
    };

    prepare_serial_log_path(manifest.serial_log.as_deref()).map_err(VmApiError::Internal)?;

    let conn = connect(connect_uri).map_err(VmApiError::Internal)?;
    let domain = Domain::define_xml_flags(&conn, &xml, sys::VIR_DOMAIN_DEFINE_VALIDATE)
        .with_context(|| format!("failed to apply VM definition {}", manifest.name))
        .map_err(VmApiError::Internal)?;

    domain_summary(&domain).map_err(VmApiError::Internal)
}

pub fn vnc_endpoint_by_name(connect_uri: &str, name: &str) -> Result<VncEndpoint> {
    let conn = connect_read_only(connect_uri)?;
    let domain = lookup_domain(&conn, name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {name} state"))?
    {
        bail!("domain {name} is not active");
    }

    query_vnc_endpoint_spec(&domain, "127.0.0.1")?
        .with_context(|| format!("domain {name} does not expose an active VNC endpoint"))
}

fn connect(uri: &str) -> Result<Connect> {
    Connect::open(Some(uri)).with_context(|| format!("failed to connect to libvirt at {uri}"))
}

fn connect_read_only(uri: &str) -> Result<Connect> {
    Connect::open_read_only(Some(uri))
        .with_context(|| format!("failed to connect to libvirt read-only at {uri}"))
}

fn lookup_domain(conn: &Connect, name: &str) -> Result<Domain> {
    Domain::lookup_by_name(conn, name).with_context(|| format!("failed to find domain {name}"))
}

fn lookup_domain_api(conn: &Connect, name: &str) -> VmApiResult<Domain> {
    match Domain::lookup_by_name(conn, name) {
        Ok(domain) => Ok(domain),
        Err(error) if error.code() == virt::error::ErrorNumber::NoDomain => {
            Err(VmApiError::NotFound(name.to_string()))
        }
        Err(error) => Err(VmApiError::Internal(
            anyhow::Error::new(error).context(format!("failed to find domain {name}")),
        )),
    }
}

fn ensure_domain_absent_api(conn: &Connect, name: &str) -> VmApiResult<()> {
    match Domain::lookup_by_name(conn, name) {
        Ok(_) => Err(VmApiError::Conflict(anyhow::anyhow!(
            "domain {name} already exists"
        ))),
        Err(error) if error.code() == virt::error::ErrorNumber::NoDomain => Ok(()),
        Err(error) => Err(VmApiError::Internal(
            anyhow::Error::new(error).context(format!("failed to check domain {name}")),
        )),
    }
}

fn start_domain(domain: &Domain, name: &str) -> Result<()> {
    if domain
        .is_active()
        .with_context(|| format!("failed to query domain {name} state"))?
    {
        eprintln!("[qtr] VM already running: {name}");
        return Ok(());
    }

    domain
        .create()
        .with_context(|| format!("failed to start domain {name}"))?;
    eprintln!("[qtr] started VM: {name}");

    Ok(())
}

fn undefine_domain(domain: &Domain, name: &str) -> Result<()> {
    domain
        .undefine()
        .with_context(|| format!("failed to undefine domain {name}"))?;
    eprintln!("[qtr] undefined VM: {name}");

    Ok(())
}

fn prepare_serial_log_path(path: Option<&Path>) -> Result<()> {
    let Some(path) = path else { return Ok(()) };
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    Ok(env::current_dir()
        .context("failed to determine current directory")?
        .join(path))
}

fn print_vnc_endpoint(domain: &Domain, fallback_listen: &str) -> Result<()> {
    match query_vnc_endpoint_spec(domain, fallback_listen)? {
        Some(endpoint) => {
            eprintln!("[qtr] VNC: {}", endpoint.display());
            if endpoint.is_wildcard() {
                let endpoints = local_vnc_endpoints(&endpoint.port);
                if !endpoints.is_empty() {
                    eprintln!("[qtr] VNC endpoints:");
                    for endpoint in endpoints {
                        eprintln!("[qtr]   {endpoint}");
                    }
                }
            }
        }
        None => eprintln!("[qtr] VNC: enabled, but port was not found in domain XML"),
    }

    Ok(())
}

fn query_vnc_endpoint(domain: &Domain, fallback_listen: &str) -> Result<Option<String>> {
    Ok(query_vnc_endpoint_spec(domain, fallback_listen)?.map(|endpoint| endpoint.display()))
}

fn query_vnc_endpoint_spec(domain: &Domain, fallback_listen: &str) -> Result<Option<VncEndpoint>> {
    let xml = domain
        .get_xml_desc(0)
        .context("failed to query domain XML")?;
    Ok(parse_vnc_endpoint(&xml, fallback_listen))
}

fn print_serial_log(domain: &Domain) -> Result<()> {
    if let Some(path) = query_serial_log(domain)? {
        eprintln!("[qtr] serial log: {path}");
    }

    Ok(())
}

fn query_serial_log(domain: &Domain) -> Result<Option<String>> {
    let xml = domain
        .get_xml_desc(0)
        .context("failed to query domain XML")?;
    Ok(parse_serial_log(&xml))
}

fn parse_serial_log(xml: &str) -> Option<String> {
    let doc = Document::parse(xml).ok()?;
    let console = doc
        .descendants()
        .find(|node| node.has_tag_name("console") && node.attribute("type") == Some("file"))?;

    optional_child(console, "source")?
        .attribute("path")
        .map(str::to_string)
}

#[derive(Clone, Debug)]
pub struct VncEndpoint {
    pub listen: String,
    pub port: String,
}

impl VncEndpoint {
    pub fn display(&self) -> String {
        format_endpoint(&self.listen, &self.port)
    }

    pub fn connect_host(&self) -> &str {
        match self.listen.as_str() {
            "0.0.0.0" => "127.0.0.1",
            "::" => "::1",
            host => host,
        }
    }

    pub fn port_number(&self) -> Result<u16> {
        self.port
            .parse()
            .with_context(|| format!("failed to parse VNC port {}", self.port))
    }

    fn is_wildcard(&self) -> bool {
        matches!(self.listen.as_str(), "0.0.0.0" | "::")
    }
}

fn parse_vnc_endpoint(xml: &str, fallback_listen: &str) -> Option<VncEndpoint> {
    let doc = Document::parse(xml).ok()?;
    let graphics = doc
        .descendants()
        .find(|node| node.has_tag_name("graphics") && node.attribute("type") == Some("vnc"))?;
    let port = graphics.attribute("port")?;
    if port == "-1" {
        return None;
    }

    let listen = graphics
        .attribute("listen")
        .map(str::to_string)
        .or_else(|| {
            optional_child(graphics, "listen")
                .and_then(|listen| listen.attribute("address"))
                .map(str::to_string)
        })
        .unwrap_or_else(|| fallback_listen.to_string());

    Some(VncEndpoint {
        listen,
        port: port.to_string(),
    })
}

fn local_vnc_endpoints(port: &str) -> Vec<String> {
    let interfaces = match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces,
        Err(_) => return Vec::new(),
    };

    let ips = interfaces
        .into_iter()
        .map(|interface| interface.ip())
        .filter(|addr| !addr.is_unspecified())
        .collect();
    format_vnc_endpoints(ips, port)
}

fn format_vnc_endpoints(ips: BTreeSet<IpAddr>, port: &str) -> Vec<String> {
    ips.into_iter()
        .map(|ip| format_endpoint(&ip.to_string(), port))
        .collect()
}

fn format_endpoint(host: &str, port: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn wait_shutdown_domain(domain: &Domain, name: &str, timeout: Option<Duration>) -> Result<()> {
    wait_for_shutdown(
        || {
            domain
                .is_active()
                .with_context(|| format!("failed to query domain {name} state"))
        },
        name,
        timeout,
    )
}

fn wait_for_shutdown(
    mut is_active: impl FnMut() -> Result<bool>,
    name: &str,
    timeout: Option<Duration>,
) -> Result<()> {
    let started = Instant::now();
    loop {
        if !is_active()? {
            return Ok(());
        }
        if let Some(timeout) = timeout
            && started.elapsed() >= timeout
        {
            bail!("timed out waiting for VM {name} to shut down");
        }

        thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_xml::{VmLaunchCdromSpec, VmLaunchMemorySpec};

    #[test]
    fn dumps_qtr_domain_xml_to_manifest() {
        let boot_devices = [BootDevice::Cdrom, BootDevice::Hd];
        let disks = [
            VmLaunchDiskSpec {
                id: None,
                path: PathBuf::from("/var/lib/libvirt/images/sys.qcow2"),
                format: DiskFormat::Qcow2,
                source: VmLaunchDiskSource::File,
                target: "vda".to_string(),
                bus: "virtio".to_string(),
                cache: None,
                io: None,
                discard: None,
                detect_zeroes: None,
                readonly: None,
                serial: None,
                io_tune: None,
                io_threads: None,
            },
            VmLaunchDiskSpec {
                id: None,
                path: PathBuf::from("/dev/disk/by-id/qtr-test-disk"),
                format: DiskFormat::Raw,
                source: VmLaunchDiskSource::Block,
                target: "sda".to_string(),
                bus: "scsi".to_string(),
                cache: Some("none"),
                io: Some("threads"),
                discard: Some("unmap"),
                detect_zeroes: Some("unmap"),
                readonly: Some(true),
                serial: Some("data-disk"),
                io_tune: Some(domain_xml::VmLaunchDiskIoTuneSpec {
                    total_bytes_per_sec: Some(10_000_000),
                    read_bytes_per_sec: None,
                    write_bytes_per_sec: None,
                    total_iops: None,
                    read_iops: Some(400),
                    write_iops: Some(100),
                }),
                io_threads: Some(VmLaunchIoThreadsSpec {
                    count: 4,
                    queues: 4,
                }),
            },
        ];
        let cdroms = [VmLaunchCdromSpec {
            id: "installer",
            media: Some(Path::new("/isos/os.iso")),
            target: "sda",
        }];
        let xml = build_vm_launch_domain_xml(VmLaunchDomainSpec {
            name: "install-os",
            machine: None,
            memory: VmLaunchMemorySpec {
                size_mib: 4096,
                max_mib: None,
            },
            vcpus: 2,
            cpu: Some(VmLaunchCpuSpec {
                mode: "host-passthrough",
                model: None,
                topology: None,
            }),
            io_threads: Some(VmLaunchIoThreadsSpec {
                count: 4,
                queues: 4,
            }),
            disks: &disks,
            cdroms: &cdroms,
            serial_log: Some(Path::new("/logs/install-os.serial.log")),
            boot_devices: &boot_devices,
            network: "default",
            graphics: GraphicsSpec {
                mode: GraphicsMode::Vnc,
                vnc_listen: "0.0.0.0",
                vnc_port: Some(5901),
            },
        });

        let manifest = manifest_from_domain_xml(&xml).expect("domain XML should parse");

        assert_eq!(manifest.name, "install-os");
        let first_disk = manifest.disks[0].as_present().unwrap();
        let second_disk = manifest.disks[1].as_present().unwrap();
        assert_eq!(
            first_disk.path,
            PathBuf::from("/var/lib/libvirt/images/sys.qcow2")
        );
        assert_eq!(first_disk.disk_type, VmDiskType::File);
        assert_eq!(first_disk.format, DiskFormat::Qcow2);
        assert_eq!(first_disk.id.as_deref(), Some("disk-vda"));
        assert_eq!(first_disk.target.as_deref(), Some("vda"));
        assert_eq!(first_disk.bus, VmDiskBus::VirtioBlk);
        assert_eq!(second_disk.disk_type, VmDiskType::Block);
        assert_eq!(
            second_disk.path,
            PathBuf::from("/dev/disk/by-id/qtr-test-disk")
        );
        assert_eq!(second_disk.format, DiskFormat::Raw);
        assert_eq!(second_disk.id.as_deref(), Some("disk-sda"));
        assert_eq!(second_disk.target.as_deref(), Some("sda"));
        assert_eq!(second_disk.bus, VmDiskBus::VirtioScsi);
        assert_eq!(second_disk.cache, Some(VmDiskCache::None));
        assert_eq!(
            second_disk.io,
            Some(VmDiskIoConfig {
                mode: VmDiskIoMode::Threads,
            })
        );
        assert_eq!(second_disk.discard, Some(VmDiskDiscard::Unmap));
        assert_eq!(second_disk.detect_zeroes, Some(VmDiskDetectZeroes::Unmap));
        assert_eq!(second_disk.readonly, Some(true));
        assert_eq!(second_disk.serial, VmDiskSerial::value("data-disk"));
        assert_eq!(
            second_disk.io_tune,
            VmDiskIoTuneConfig::configured(VmDiskIoTune {
                total_bytes_per_sec: Some(10_000_000),
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
                total_iops: None,
                read_iops: Some(400),
                write_iops: Some(100),
            })
        );
        assert_eq!(
            manifest.io_threads,
            Some(VmIoThreads {
                count: 4,
                queues: None,
            })
        );
        assert!(xml.contains("<iothreads>4</iothreads>"));
        assert!(xml.contains("<controller type='scsi' index='0' model='virtio-scsi'>"));
        let cdrom = manifest.cdroms.as_ref().unwrap()[0].as_present().unwrap();
        assert_eq!(cdrom.id, "installer");
        assert_eq!(cdrom.media, Some(PathBuf::from("/isos/os.iso")));
        assert_eq!(cdrom.target.as_deref(), Some("sda"));
        assert_eq!(
            manifest.boot,
            Some(vec!["cdrom".to_string(), "hd".to_string()])
        );
        assert_eq!(manifest.memory_gib, 4);
        assert_eq!(manifest.vcpus, 2);
        assert_eq!(
            manifest.memory,
            Some(VmMemory {
                size_mib: 4096,
                max_mib: None,
            })
        );
        assert_eq!(
            manifest.cpu,
            Some(VmCpu {
                mode: VmCpuMode::HostPassthrough,
                model: None,
                vcpus: Some(2),
                topology: None,
            })
        );
        assert_eq!(manifest.network, "default");
        assert_eq!(manifest.graphics, GraphicsMode::Vnc);
        assert_eq!(manifest.vnc_listen, "0.0.0.0");
        assert_eq!(manifest.vnc_port, Some(5901));
        assert_eq!(
            manifest.serial_log,
            Some(PathBuf::from("/logs/install-os.serial.log"))
        );
    }

    #[test]
    fn patches_existing_domain_xml_without_rebuilding_it() {
        let xml = r#"<domain type='kvm'>
  <name>install-os</name>
  <uuid>c194be5c-a0ba-4e90-8b23-18c8df0825f1</uuid>
  <memory unit='KiB'>4194304</memory>
  <currentMemory unit='KiB'>4194304</currentMemory>
  <vcpu placement='static'>2</vcpu>
  <os>
    <type arch='x86_64' machine='pc-i440fx-10.2'>hvm</type>
    <boot dev='cdrom'/>
    <boot dev='hd'/>
  </os>
  <features>
    <acpi/>
    <apic/>
  </features>
  <cpu mode='host-passthrough' check='none' migratable='off'/>
  <devices>
    <emulator>/usr/bin/qemu-system-x86_64</emulator>
    <disk type='file' device='disk'>
      <driver name='qemu' type='qcow2'/>
      <source file='/fixtures/qtr/disks/sys.qcow2'/>
      <target dev='vda' bus='virtio'/>
      <address type='pci' domain='0x0000' bus='0x00' slot='0x07' function='0x0'/>
    </disk>
    <disk type='file' device='cdrom'>
      <driver name='qemu' type='raw'/>
      <source file='/fixtures/qtr/iso/CentOS-7-x86_64-DVD-2207-02.iso'/>
      <target dev='sda' bus='sata'/>
      <readonly/>
      <address type='drive' controller='0' bus='0' target='0' unit='0'/>
    </disk>
    <controller type='usb' index='0' model='qemu-xhci'>
      <address type='pci' domain='0x0000' bus='0x00' slot='0x04' function='0x0'/>
    </controller>
    <interface type='network'>
      <mac address='52:54:00:1c:92:5f'/>
      <source network='default'/>
      <model type='virtio'/>
      <address type='pci' domain='0x0000' bus='0x00' slot='0x03' function='0x0'/>
    </interface>
    <serial type='file'>
      <source path='/fixtures/qtr/logs/install-os.serial.log'/>
      <target type='isa-serial' port='0'>
        <model name='isa-serial'/>
      </target>
    </serial>
    <console type='file'>
      <source path='/fixtures/qtr/logs/install-os.serial.log'/>
      <target type='serial' port='0'/>
    </console>
    <channel type='unix'>
      <target type='virtio' name='org.qemu.guest_agent.0'/>
      <address type='virtio-serial' controller='0' bus='0' port='1'/>
    </channel>
    <input type='tablet' bus='usb'>
      <address type='usb' bus='0' port='1'/>
    </input>
    <graphics type='vnc' port='-1' autoport='yes' listen='0.0.0.0'>
      <listen type='address' address='0.0.0.0'/>
    </graphics>
    <video>
      <model type='cirrus' vram='16384' heads='1' primary='yes'/>
      <address type='pci' domain='0x0000' bus='0x00' slot='0x02' function='0x0'/>
    </video>
  </devices>
</domain>
"#;
        let manifest = VmManifest {
            name: "install-os".to_string(),
            machine: None,
            cpu: None,
            memory: None,
            io_threads: None,
            disks: vec![VmDiskEntry::present(VmDisk {
                id: None,
                disk_type: VmDiskType::File,
                path: PathBuf::from("/fixtures/qtr/disks/sys.qcow2"),
                format: DiskFormat::Qcow2,
                target: Some("vda".to_string()),
                bus: VmDiskBus::VirtioBlk,
                cache: Some(VmDiskCache::None),
                io: Some(VmDiskIoConfig {
                    mode: VmDiskIoMode::Native,
                }),
                discard: None,
                detect_zeroes: None,
                readonly: None,
                serial: VmDiskSerial::default(),
                io_tune: VmDiskIoTuneConfig::default(),
            })],
            cdrom: Some(PathBuf::from(
                "/fixtures/qtr/iso/CentOS-7-x86_64-DVD-2207-02.iso",
            )),
            cdroms: None,
            boot: Some(vec!["hd".to_string()]),
            memory_gib: 4,
            vcpus: 2,
            network: "default".to_string(),
            graphics: GraphicsMode::Vnc,
            vnc_listen: "0.0.0.0".to_string(),
            vnc_port: None,
            serial_log: Some(PathBuf::from("/fixtures/qtr/logs/install-os.serial.log")),
        };
        let boot_devices = [BootDevice::Hd];

        let patched = patch_domain_xml(xml, &manifest, &boot_devices).expect("XML should patch");

        assert!(patched.contains("<uuid>c194be5c-a0ba-4e90-8b23-18c8df0825f1</uuid>"));
        assert!(patched.contains("machine='pc-i440fx-10.2'"));
        assert!(patched.contains("<memory unit='KiB'>4194304</memory>"));
        assert!(patched.contains("<driver name='qemu' type='qcow2' cache='none' io='native'/>"));
        assert!(patched.contains(
            "<address type='pci' domain='0x0000' bus='0x00' slot='0x07' function='0x0'/>"
        ));
        assert!(patched.contains("<video>"));
        assert!(!patched.contains("<boot dev='cdrom'/>"));
        assert!(patched.contains("    <boot dev='hd'/>\n"));
    }

    #[test]
    fn preserves_opaque_disk_xml_when_patching_managed_fields() {
        let xml = test_domain_xml().replace(
            "    <disk type='file' device='disk'>\n      <driver name='qemu' type='qcow2'/>\n      <source file='/vm/sys.qcow2'/>\n      <target dev='vda' bus='virtio'/>\n      <address type='pci' domain='0x0000' bus='0x00' slot='0x07' function='0x0'/>\n    </disk>",
            "    <disk type='file' device='disk' snapshot='external'>\n      <driver name='qemu' type='qcow2' cache='writeback' io='threads' queues='8' error_policy='stop'/>\n      <auth username='storage-user'>\n        <secret type='ceph' usage='qtr-test'/>\n      </auth>\n      <source file='/vm/sys.qcow2' startupPolicy='optional'>\n        <seclabel model='dac' relabel='no'/>\n      </source>\n      <backingStore type='file'>\n        <format type='qcow2'/>\n        <source file='/vm/base.qcow2'/>\n      </backingStore>\n      <target dev='vda' bus='virtio' rotation_rate='1'/>\n      <readonly/>\n      <serial>root-disk</serial>\n      <iotune><read_iops_sec>1000</read_iops_sec></iotune>\n      <encryption format='luks'>\n        <secret type='passphrase' usage='qtr-luks'/>\n      </encryption>\n      <boot order='1'/>\n      <alias name='ua-qtr-disk-root'/>\n      <address type='pci' domain='0x0000' bus='0x00' slot='0x07' function='0x0'/>\n    </disk>",
        );
        let mut disk = test_file_disk("/vm/sys.qcow2", Some("vda"), VmDiskBus::VirtioBlk);
        disk.cache = Some(VmDiskCache::None);
        disk.io = Some(VmDiskIoConfig {
            mode: VmDiskIoMode::Native,
        });
        let manifest = test_manifest(vec![disk]);

        let patched = patch_domain_xml(&xml, &manifest, &[BootDevice::Hd])
            .expect("opaque disk XML should survive a managed update");

        for expected in [
            "snapshot='external'",
            "error_policy='stop'",
            "<auth username='storage-user'>",
            "<secret type='ceph' usage='qtr-test'/>",
            "startupPolicy='optional'",
            "<seclabel model='dac' relabel='no'/>",
            "<backingStore type='file'>",
            "<source file='/vm/base.qcow2'/>",
            "rotation_rate='1'",
            "<readonly/>",
            "<serial>root-disk</serial>",
            "<iotune><read_iops_sec>1000</read_iops_sec></iotune>",
            "<encryption format='luks'>",
            "<boot order='1'/>",
            "<alias name='ua-qtr-disk-root'/>",
            "<address type='pci' domain='0x0000' bus='0x00' slot='0x07' function='0x0'/>",
        ] {
            assert!(patched.contains(expected), "missing {expected}");
        }
        assert!(patched.contains("cache='none' io='native'"));
        assert!(!patched.contains("cache='writeback'"));
        assert!(!patched.contains("queues='8'"));

        let patched_again = patch_domain_xml(&patched, &manifest, &[BootDevice::Hd])
            .expect("second opaque disk patch should succeed");
        assert_eq!(patched_again, patched);
    }

    #[test]
    fn reconciles_advanced_disk_options_explicitly() {
        let xml = test_domain_xml().replace(
            "<driver name='qemu' type='qcow2'/>",
            "<driver name='qemu' type='qcow2' discard='ignore' detect_zeroes='off' error_policy='stop'/>",
        ).replace(
            "      <address type='pci'",
            "      <readonly/>\n      <serial>old-disk</serial>\n      <address type='pci'",
        );
        let mut disk = test_file_disk("/vm/sys.qcow2", Some("vda"), VmDiskBus::VirtioBlk);
        disk.discard = Some(VmDiskDiscard::Unmap);
        disk.detect_zeroes = Some(VmDiskDetectZeroes::Unmap);
        disk.readonly = Some(false);
        disk.serial = VmDiskSerial::Remove;
        let manifest = test_manifest(vec![disk]);

        let removed = patch_domain_xml(&xml, &manifest, &[BootDevice::Hd])
            .expect("advanced disk options should reconcile");
        assert!(removed.contains("discard='unmap' detect_zeroes='unmap' error_policy='stop'"));
        assert!(!removed.contains("<readonly/>"));
        assert!(!removed.contains("<serial>"));

        let mut disk = manifest.disks[0].as_present().unwrap().clone();
        disk.readonly = Some(true);
        disk.serial = VmDiskSerial::value("new&disk");
        let manifest = test_manifest(vec![disk]);
        let added = patch_domain_xml(&removed, &manifest, &[BootDevice::Hd])
            .expect("advanced disk options should be added");
        assert!(added.contains("<readonly/>"));
        assert!(added.contains("<serial>new&amp;disk</serial>"));

        let added_again = patch_domain_xml(&added, &manifest, &[BootDevice::Hd])
            .expect("advanced disk reconciliation should be idempotent");
        assert_eq!(added_again, added);
    }

    #[test]
    fn reconciles_disk_iotune_and_preserves_unknown_limits() {
        let xml = test_domain_xml().replace(
            "      <address type='pci'",
            "      <iotune>\n        <read_bytes_sec>500000</read_bytes_sec>\n        <read_bytes_sec_max>2000000</read_bytes_sec_max>\n      </iotune>\n      <address type='pci'",
        );
        let mut disk = test_file_disk("/vm/sys.qcow2", Some("vda"), VmDiskBus::VirtioBlk);
        disk.io_tune = VmDiskIoTuneConfig::configured(VmDiskIoTune {
            total_bytes_per_sec: None,
            read_bytes_per_sec: Some(750_000),
            write_bytes_per_sec: Some(250_000),
            total_iops: Some(1_000),
            read_iops: None,
            write_iops: None,
        });
        let manifest = test_manifest(vec![disk]);

        let patched = patch_domain_xml(&xml, &manifest, &[BootDevice::Hd])
            .expect("disk IO limits should reconcile");
        assert!(patched.contains("<read_bytes_sec>750000</read_bytes_sec>"));
        assert!(patched.contains("<write_bytes_sec>250000</write_bytes_sec>"));
        assert!(patched.contains("<total_iops_sec>1000</total_iops_sec>"));
        assert!(patched.contains("<read_bytes_sec_max>2000000</read_bytes_sec_max>"));

        let patched_again = patch_domain_xml(&patched, &manifest, &[BootDevice::Hd])
            .expect("disk IO limit reconciliation should be idempotent");
        assert_eq!(patched_again, patched);

        let mut disk = manifest.disks[0].as_present().unwrap().clone();
        disk.io_tune = VmDiskIoTuneConfig::Remove;
        let removed = patch_domain_xml(&patched, &test_manifest(vec![disk]), &[BootDevice::Hd])
            .expect("disk IO limits should be removable");
        assert!(!removed.contains("<iotune>"));
        assert!(!removed.contains("read_bytes_sec_max"));
    }

    #[test]
    fn patches_machine_cpu_topology_and_memory_idempotently() {
        let xml = test_domain_xml().replace(
            "  <devices>",
            "  <cpu mode='custom' deprecated='no'>\n    <model fallback='allow'>OldModel</model>\n    <vendor>AuthenticAMD</vendor>\n    <feature policy='require' name='aes'/>\n  </cpu>\n  <devices>",
        );
        let mut manifest = test_manifest(vec![test_file_disk(
            "/vm/sys.qcow2",
            None,
            VmDiskBus::VirtioBlk,
        )]);
        manifest.machine = Some(VmMachine {
            machine_type: "pc-q35-10.0".to_string(),
        });
        manifest.cpu = Some(VmCpu {
            mode: VmCpuMode::HostModel,
            model: None,
            vcpus: None,
            topology: Some(VmCpuTopology {
                sockets: 2,
                cores: 2,
                threads: 2,
            }),
        });
        manifest.memory = Some(VmMemory {
            size_mib: 2048,
            max_mib: Some(8192),
        });

        let patched = patch_domain_xml(&xml, &manifest, &[BootDevice::Hd])
            .expect("resource configuration should patch");

        assert!(patched.contains("<memory unit='MiB'>8192</memory>"));
        assert!(patched.contains("<currentMemory unit='MiB'>2048</currentMemory>"));
        assert!(patched.contains("<vcpu placement='static'>8</vcpu>"));
        assert!(patched.contains("<type arch='x86_64' machine='pc-q35-10.0'>hvm</type>"));
        assert!(patched.contains("<cpu mode='host-model' deprecated='no'>"));
        assert!(patched.contains("<topology sockets='2' cores='2' threads='2'/>"));
        assert!(patched.contains("<feature policy='require' name='aes'/>"));
        assert!(!patched.contains("OldModel"));
        let vendor = patched
            .find("<vendor>AuthenticAMD</vendor>")
            .expect("vendor should be preserved");
        let topology = patched
            .find("<topology sockets=")
            .expect("topology should be generated");
        assert!(vendor < topology);

        let patched_again = patch_domain_xml(&patched, &manifest, &[BootDevice::Hd])
            .expect("second resource patch should succeed");
        assert_eq!(patched_again, patched);
    }

    #[test]
    fn dumps_non_gib_memory_and_cpu_topology() {
        let xml = test_domain_xml()
            .replace("<memory unit='MiB'>4096</memory>", "<memory unit='MiB'>6144</memory>")
            .replace(
                "<currentMemory unit='MiB'>4096</currentMemory>",
                "<currentMemory unit='MiB'>1536</currentMemory>",
            )
            .replace(
                "  <devices>",
                "  <cpu mode='host-model'>\n    <topology sockets='1' cores='2' threads='1'/>\n  </cpu>\n  <devices>",
            );

        let manifest = manifest_from_domain_xml(&xml).expect("domain XML should dump");
        let yaml = serialize_manifest_yaml(&manifest).expect("manifest should serialize");

        assert_eq!(
            manifest.memory,
            Some(VmMemory {
                size_mib: 1536,
                max_mib: Some(6144),
            })
        );
        assert_eq!(
            manifest.cpu,
            Some(VmCpu {
                mode: VmCpuMode::HostModel,
                model: None,
                vcpus: None,
                topology: Some(VmCpuTopology {
                    sockets: 1,
                    cores: 2,
                    threads: 1,
                }),
            })
        );
        assert!(yaml.contains("sizeMiB: 1536"));
        assert!(yaml.contains("maxMiB: 6144"));
        assert!(!yaml.contains("memoryGiB"));
        assert!(!yaml.contains("\nvcpus:"));
    }

    #[test]
    fn preserves_existing_disk_target_when_manifest_order_changes() {
        let manifest = test_manifest(vec![
            test_file_disk("/vm/data.qcow2", None, VmDiskBus::VirtioBlk),
            test_file_disk("/vm/sys.qcow2", None, VmDiskBus::VirtioBlk),
        ]);

        let patched = patch_domain_xml(test_domain_xml(), &manifest, &[BootDevice::Hd])
            .expect("XML should patch");

        assert_eq!(patched.matches("device='disk'").count(), 2);
        assert!(
            patched
                .contains("<source file='/vm/sys.qcow2'/>\n      <target dev='vda' bus='virtio'/>")
        );
        assert!(
            patched.contains(
                "<source file='/vm/data.qcow2'/>\n      <target dev='vdb' bus='virtio'/>"
            )
        );
    }

    #[test]
    fn appends_virtio_scsi_disk_with_controller() {
        let manifest = test_manifest(vec![
            test_file_disk("/vm/sys.qcow2", None, VmDiskBus::VirtioBlk),
            test_file_disk("/vm/scsi.qcow2", None, VmDiskBus::VirtioScsi),
        ]);

        let patched = patch_domain_xml(test_domain_xml(), &manifest, &[BootDevice::Hd])
            .expect("XML should patch");

        assert!(
            patched
                .contains("<source file='/vm/sys.qcow2'/>\n      <target dev='vda' bus='virtio'/>")
        );
        assert!(
            patched
                .contains("<source file='/vm/scsi.qcow2'/>\n      <target dev='sda' bus='scsi'/>")
        );
        assert!(patched.contains("<controller type='scsi' index='0' model='virtio-scsi'/>"));
    }

    #[test]
    fn assign_targets_skips_cdrom_target() {
        let mut manifest = test_manifest(vec![test_file_disk(
            "/vm/scsi.qcow2",
            None,
            VmDiskBus::VirtioScsi,
        )]);
        manifest.cdrom = Some(PathBuf::from("/isos/os.iso"));

        let targets = assign_manifest_disk_targets(&manifest).expect("targets should assign");
        assert_eq!(targets, vec!["sdb".to_string()]);
    }

    #[test]
    fn assign_targets_rejects_explicit_cdrom_target_conflict() {
        let mut manifest = test_manifest(vec![test_file_disk(
            "/vm/scsi.qcow2",
            Some("sda"),
            VmDiskBus::VirtioScsi,
        )]);
        manifest.cdrom = Some(PathBuf::from("/isos/os.iso"));

        let err = assign_manifest_disk_targets(&manifest).unwrap_err();
        assert!(err.to_string().contains("duplicate disk target sda"));
    }

    #[test]
    fn builds_domain_xml_without_target_conflict_with_cdrom_and_scsi() {
        let mut manifest = test_manifest(vec![test_file_disk(
            "/vm/scsi.qcow2",
            None,
            VmDiskBus::VirtioScsi,
        )]);
        manifest.cdrom = Some(PathBuf::from("/isos/os.iso"));

        let xml = build_manifest_domain_xml(&manifest, &[BootDevice::Cdrom, BootDevice::Hd])
            .expect("domain XML should build");

        assert!(xml.contains("<target dev='sda' bus='sata'/>"));
        assert!(xml.contains("<target dev='sdb' bus='scsi'/>"));
        assert!(!xml.contains("<target dev='sda' bus='scsi'/>"));
    }

    #[test]
    fn builds_multiple_cdroms_and_reserves_scsi_targets() {
        let mut manifest = test_manifest(vec![test_file_disk(
            "/vm/scsi.qcow2",
            None,
            VmDiskBus::VirtioScsi,
        )]);
        manifest.cdroms = Some(vec![
            VmCdromEntry::present(VmCdrom {
                id: "installer".to_string(),
                media: Some(PathBuf::from("/isos/os.iso")),
                target: None,
            }),
            VmCdromEntry::present(VmCdrom {
                id: "tools".to_string(),
                media: None,
                target: None,
            }),
        ]);

        let xml = build_manifest_domain_xml(&manifest, &[BootDevice::Cdrom, BootDevice::Hd])
            .expect("multi-CD-ROM domain XML should build");

        assert!(xml.contains("<target dev='sda' bus='sata'/>"));
        assert!(xml.contains("<target dev='sdb' bus='sata'/>"));
        assert!(xml.contains("<target dev='sdc' bus='scsi'/>"));
        assert!(xml.contains("<alias name='ua-qtr-cdrom-installer'/>"));
        assert!(xml.contains("<alias name='ua-qtr-cdrom-tools'/>"));
        assert_eq!(xml.matches("device='cdrom'").count(), 2);
    }

    #[test]
    fn appended_scsi_disk_skips_existing_cdrom_target() {
        let xml_with_cdrom = test_domain_xml().replace(
            "    <interface type='network'>",
            "    <disk type='file' device='cdrom'>\n      <driver name='qemu' type='raw'/>\n      <source file='/isos/os.iso'/>\n      <target dev='sda' bus='sata'/>\n      <readonly/>\n    </disk>\n    <interface type='network'>",
        );
        let manifest = test_manifest(vec![
            test_file_disk("/vm/sys.qcow2", None, VmDiskBus::VirtioBlk),
            test_file_disk("/vm/scsi.qcow2", None, VmDiskBus::VirtioScsi),
        ]);

        let patched = patch_domain_xml(&xml_with_cdrom, &manifest, &[BootDevice::Hd])
            .expect("XML should patch");

        assert!(
            patched
                .contains("<source file='/vm/scsi.qcow2'/>\n      <target dev='sdb' bus='scsi'/>")
        );
    }

    #[test]
    fn reconciles_cdrom_media_empty_trays_and_detach() {
        let current_cdrom = domain_xml::build_cdrom_xml(&domain_xml::VmLaunchCdromSpec {
            id: "installer",
            media: Some(Path::new("/isos/old.iso")),
            target: "sda",
        })
        .replace(
            "<source file='/isos/old.iso'/>",
            "<source file='/isos/old.iso' startupPolicy='optional'/>",
        )
        .replace(
            "    </disk>",
            "      <address type='drive' controller='0' bus='0' target='0' unit='0'/>\n    </disk>",
        );
        let xml = test_domain_xml().replace(
            "    <interface type='network'>",
            &format!("{current_cdrom}    <interface type='network'>"),
        );
        let mut manifest = test_manifest(vec![test_file_disk(
            "/vm/sys.qcow2",
            None,
            VmDiskBus::VirtioBlk,
        )]);
        manifest.cdroms = Some(vec![
            VmCdromEntry::present(VmCdrom {
                id: "installer".to_string(),
                media: Some(PathBuf::from("/isos/new.iso")),
                target: None,
            }),
            VmCdromEntry::present(VmCdrom {
                id: "tools".to_string(),
                media: None,
                target: None,
            }),
        ]);

        let changed = patch_domain_xml(&xml, &manifest, &[BootDevice::Hd])
            .expect("CD-ROM media should reconcile");
        assert!(changed.contains("<source file='/isos/new.iso' startupPolicy='optional'/>"));
        assert!(changed.contains("<target dev='sda' bus='sata'/>"));
        assert!(changed.contains("<target dev='sdb' bus='sata'/>"));
        assert!(changed.contains("<alias name='ua-qtr-cdrom-tools'/>"));
        assert!(changed.contains("<address type='drive' controller='0'"));
        assert_eq!(changed.matches("device='cdrom'").count(), 2);

        let changed_again = patch_domain_xml(&changed, &manifest, &[BootDevice::Hd])
            .expect("CD-ROM reconciliation should be idempotent");
        assert_eq!(changed_again, changed);

        manifest.cdroms = Some(vec![
            VmCdromEntry::present(VmCdrom {
                id: "installer".to_string(),
                media: None,
                target: None,
            }),
            VmCdromEntry::present(VmCdrom {
                id: "tools".to_string(),
                media: None,
                target: None,
            }),
        ]);
        let ejected = patch_domain_xml(&changed, &manifest, &[BootDevice::Hd])
            .expect("CD-ROM media should eject");
        assert!(!ejected.contains("/isos/new.iso"));
        assert!(ejected.contains("ua-qtr-cdrom-installer"));

        manifest.cdroms = Some(vec![
            VmCdromEntry::absent("installer"),
            VmCdromEntry::present(VmCdrom {
                id: "tools".to_string(),
                media: None,
                target: None,
            }),
        ]);
        let detached = patch_domain_xml(&ejected, &manifest, &[BootDevice::Hd])
            .expect("CD-ROM should detach persistently");
        assert!(!detached.contains("ua-qtr-cdrom-installer"));
        assert!(detached.contains("ua-qtr-cdrom-tools"));
        assert_eq!(detached.matches("device='cdrom'").count(), 1);
    }

    #[test]
    fn builds_virtio_blk_iothread_mapping() {
        let mut manifest = test_manifest(vec![test_file_disk(
            "/vm/sys.qcow2",
            None,
            VmDiskBus::VirtioBlk,
        )]);
        manifest.io_threads = Some(VmIoThreads {
            count: 2,
            queues: Some(4),
        });
        manifest.disks[0].as_present_mut().unwrap().io = Some(VmDiskIoConfig {
            mode: VmDiskIoMode::Threads,
        });

        let xml = build_manifest_domain_xml(&manifest, &[BootDevice::Hd])
            .expect("domain XML should build");

        assert!(xml.contains("<iothreads>2</iothreads>"));
        assert!(xml.contains("<driver name='qemu' type='qcow2' io='threads' queues='4'>"));
        assert!(xml.contains("<iothread id='1'>"));
        assert!(xml.contains("<queue id='0'/>"));
        assert!(xml.contains("<queue id='2'/>"));
        assert!(xml.contains("<iothread id='2'>"));
        assert!(xml.contains("<queue id='1'/>"));
        assert!(xml.contains("<queue id='3'/>"));
    }

    #[test]
    fn patches_virtio_scsi_controller_iothread_mapping() {
        let mut manifest = test_manifest(vec![
            test_file_disk("/vm/sys.qcow2", None, VmDiskBus::VirtioBlk),
            test_file_disk("/vm/scsi.qcow2", None, VmDiskBus::VirtioScsi),
        ]);
        manifest.io_threads = Some(VmIoThreads {
            count: 2,
            queues: None,
        });
        manifest.disks[1].as_present_mut().unwrap().io = Some(VmDiskIoConfig {
            mode: VmDiskIoMode::Threads,
        });

        let patched = patch_domain_xml(test_domain_xml(), &manifest, &[BootDevice::Hd])
            .expect("XML should patch");

        assert!(patched.contains("<iothreads>2</iothreads>"));
        assert!(patched.contains("<controller type='scsi' index='0' model='virtio-scsi'>"));
        assert!(patched.contains("<driver queues='2'>"));
        assert!(patched.contains("<iothread id='1'>"));
        assert!(patched.contains("<queue id='0'/>"));
        assert!(patched.contains("<iothread id='2'>"));
        assert!(patched.contains("<queue id='1'/>"));
    }

    #[test]
    fn rejects_legacy_disk_io_scalar_and_queues() {
        let legacy_io = r#"name: install-os
disks:
- path: /tmp/sys.qcow2
  type: file
  format: qcow2
  io: threads
"#;
        let legacy_queues = r#"name: install-os
disks:
- path: /tmp/sys.qcow2
  type: file
  format: qcow2
  queues: 1
"#;

        assert!(serde_yaml::from_str::<VmManifest>(legacy_io).is_err());
        assert!(serde_yaml::from_str::<VmManifest>(legacy_queues).is_err());
    }

    #[test]
    fn rejects_implicit_disk_removal() {
        let manifest = test_manifest(Vec::new());

        let error = patch_domain_xml(test_domain_xml(), &manifest, &[BootDevice::Hd])
            .expect_err("implicit disk removal should be rejected");

        assert!(error.to_string().contains("add state: absent"));
    }

    #[test]
    fn rejects_replacing_disk_path_without_explicit_target() {
        let manifest = test_manifest(vec![test_file_disk(
            "/vm/replacement.qcow2",
            None,
            VmDiskBus::VirtioBlk,
        )]);

        let error = patch_domain_xml(test_domain_xml(), &manifest, &[BootDevice::Hd])
            .expect_err("ambiguous disk replacement should be rejected");

        assert!(error.to_string().contains("existing disk target vda"));
        assert!(error.to_string().contains("stable id or target"));
    }

    #[test]
    fn explicitly_detaches_persistent_disk_without_deleting_storage() {
        let dir = TestDiskDir::new();
        let disk_path = dir.create_disk("sys.qcow2");
        let xml = test_domain_xml().replace("/vm/sys.qcow2", disk_path.to_str().unwrap());
        let mut manifest = test_manifest(Vec::new());
        manifest.disks = vec![VmDiskEntry::absent("disk-vda")];

        let patched = patch_domain_xml(&xml, &manifest, &[BootDevice::Hd])
            .expect("explicit disk detach should patch persistent XML");

        assert!(!patched.contains("device='disk'"));
        assert!(disk_path.exists());

        let patched_again = patch_domain_xml(&patched, &manifest, &[BootDevice::Hd])
            .expect("repeated detach should be idempotent");
        assert_eq!(patched_again, patched);
        assert!(disk_path.exists());
    }

    #[test]
    fn detach_releases_target_for_new_disk() {
        let mut replacement =
            test_file_disk("/vm/replacement.qcow2", Some("vda"), VmDiskBus::VirtioBlk);
        replacement.id = Some("replacement".to_string());
        let mut manifest = test_manifest(Vec::new());
        manifest.disks = vec![
            VmDiskEntry::absent("disk-vda"),
            VmDiskEntry::present(replacement),
        ];

        let patched = patch_domain_xml(test_domain_xml(), &manifest, &[BootDevice::Hd])
            .expect("detached target should be reusable");

        assert!(patched.contains("<source file='/vm/replacement.qcow2'/>"));
        assert!(patched.contains("<target dev='vda' bus='virtio'/>"));
        assert!(patched.contains("<alias name='ua-qtr-disk-replacement'/>"));
        assert_eq!(patched.matches("device='disk'").count(), 1);
    }

    #[test]
    fn replaces_disk_path_with_explicit_target() {
        let manifest = test_manifest(vec![test_file_disk(
            "/vm/replacement.qcow2",
            Some("vda"),
            VmDiskBus::VirtioBlk,
        )]);

        let patched = patch_domain_xml(test_domain_xml(), &manifest, &[BootDevice::Hd])
            .expect("explicit disk replacement should patch");

        assert!(patched.contains("<source file='/vm/replacement.qcow2'/>"));
        assert!(!patched.contains("<source file='/vm/sys.qcow2'/>"));
        assert_eq!(patched.matches("device='disk'").count(), 1);
    }

    #[test]
    fn replaces_disk_path_by_stable_id() {
        let xml = test_domain_xml().replace(
            "      <address type='pci'",
            "      <alias name='ua-qtr-disk-root'/>
      <address type='pci'",
        );
        let mut disk = test_file_disk("/vm/replacement.qcow2", None, VmDiskBus::VirtioBlk);
        disk.id = Some("root".to_string());
        let manifest = test_manifest(vec![disk]);

        let patched = patch_domain_xml(&xml, &manifest, &[BootDevice::Hd])
            .expect("stable disk id should identify a changed source");

        assert!(patched.contains("<source file='/vm/replacement.qcow2'/>"));
        assert!(patched.contains("<target dev='vda' bus='virtio'/>"));
        assert!(patched.contains("<alias name='ua-qtr-disk-root'/>"));
    }

    #[test]
    fn adopts_stable_disk_id_without_overwriting_foreign_alias() {
        let mut disk = test_file_disk("/vm/sys.qcow2", Some("vda"), VmDiskBus::VirtioBlk);
        disk.id = Some("root".to_string());
        let manifest = test_manifest(vec![disk.clone()]);

        let adopted = patch_domain_xml(test_domain_xml(), &manifest, &[BootDevice::Hd])
            .expect("disk id should add an alias");
        assert!(adopted.contains("<alias name='ua-qtr-disk-root'/>"));

        let adopted_again = patch_domain_xml(&adopted, &manifest, &[BootDevice::Hd])
            .expect("adopted disk id should remain stable");
        assert_eq!(adopted_again, adopted);

        let foreign = test_domain_xml().replace(
            "      <address type='pci'",
            "      <alias name='ua-external-root'/>
      <address type='pci'",
        );
        let preserved = patch_domain_xml(&foreign, &manifest, &[BootDevice::Hd])
            .expect("foreign alias should be preserved");
        assert!(preserved.contains("<alias name='ua-external-root'/>"));
        assert!(!preserved.contains("ua-qtr-disk-root"));
    }

    #[test]
    fn leaves_serial_log_unconfigured_when_manifest_omits_it() {
        let mut manifest = VmManifest {
            name: "install-os".to_string(),
            machine: None,
            cpu: None,
            memory: None,
            io_threads: None,
            disks: vec![VmDiskEntry::present(VmDisk {
                id: None,
                disk_type: VmDiskType::File,
                path: PathBuf::from("sys.qcow2"),
                format: DiskFormat::Qcow2,
                target: None,
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
            cdroms: None,
            boot: Some(vec!["hd".to_string()]),
            memory_gib: 4,
            vcpus: 2,
            network: "default".to_string(),
            graphics: GraphicsMode::Vnc,
            vnc_listen: "127.0.0.1".to_string(),
            vnc_port: None,
            serial_log: None,
        };

        normalize_manifest_paths(&mut manifest, Path::new("/tmp/qtr"))
            .expect("manifest paths should normalize");
        let xml = build_manifest_domain_xml(&manifest, &[BootDevice::Hd])
            .expect("domain XML should build");

        assert_eq!(manifest.serial_log, None);
        assert!(xml.contains("<console type='pty'>"));
        assert!(!xml.contains(".serial.log"));
        assert_eq!(parse_serial_log(&xml), None);
    }

    #[test]
    fn parses_legacy_virtio_bus_as_virtio_blk() {
        let yaml = r#"name: install-os
disks:
- path: /tmp/sys.qcow2
  type: file
  format: qcow2
  bus: virtio
memoryGiB: 4
vcpus: 2
network: default
graphics: vnc
vncListen: 127.0.0.1
"#;

        let manifest: VmManifest = serde_yaml::from_str(yaml).expect("legacy YAML should parse");

        assert_eq!(
            manifest.disks[0].as_present().unwrap().bus,
            VmDiskBus::VirtioBlk
        );
    }

    #[test]
    fn parses_versioned_and_unversioned_manifest_documents() {
        let unversioned = r#"name: install-os
disks:
- path: /tmp/sys.qcow2
  format: qcow2
"#;
        let versioned = format!("schemaVersion: 1\n{unversioned}");

        let legacy = parse_manifest_yaml(unversioned).expect("unversioned manifest should parse");
        let current = parse_manifest_yaml(&versioned).expect("versioned manifest should parse");

        assert_eq!(legacy.name, current.name);
        assert_eq!(
            legacy.disks[0].as_present().unwrap().path,
            current.disks[0].as_present().unwrap().path
        );
    }

    #[test]
    fn serializes_current_manifest_schema_version_first() {
        let yaml = serialize_manifest_yaml(&test_manifest(vec![test_file_disk(
            "/vm/sys.qcow2",
            Some("vda"),
            VmDiskBus::VirtioBlk,
        )]))
        .expect("manifest should serialize");

        assert!(yaml.starts_with("schemaVersion: 2\n"));
        assert!(yaml.contains("name: install-os\n"));
    }

    #[test]
    fn parses_and_serializes_absent_disk_tombstone() {
        let yaml = "schemaVersion: 2\nname: vm\ndisks:\n- id: data\n  state: absent\n";

        let manifest = parse_manifest_yaml(yaml).expect("absent disk should parse");
        let output = serialize_manifest_yaml(&manifest).expect("absent disk should serialize");

        assert_eq!(manifest.disks[0].absent_id(), Some("data"));
        assert!(output.starts_with("schemaVersion: 2\n"));
        assert!(output.contains("- id: data\n  state: absent\n"));
        assert!(!output.contains("path:"));
    }

    #[test]
    fn parses_and_serializes_advanced_disk_options() {
        let yaml = r#"schemaVersion: 2
name: vm
disks:
- id: root
  path: /tmp/root.qcow2
  format: qcow2
  discard: unmap
  detectZeroes: on
  readonly: false
  serial: null
  ioTune:
    totalBytesPerSec: 10000000
    readIops: 400
    writeIops: 100
"#;

        let manifest = parse_manifest_yaml(yaml).expect("advanced disk options should parse");
        let disk = manifest.disks[0].as_present().unwrap();
        assert_eq!(disk.discard, Some(VmDiskDiscard::Unmap));
        assert_eq!(disk.detect_zeroes, Some(VmDiskDetectZeroes::On));
        assert_eq!(disk.readonly, Some(false));
        assert_eq!(disk.serial, VmDiskSerial::Remove);
        assert_eq!(
            disk.io_tune,
            VmDiskIoTuneConfig::configured(VmDiskIoTune {
                total_bytes_per_sec: Some(10_000_000),
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
                total_iops: None,
                read_iops: Some(400),
                write_iops: Some(100),
            })
        );

        let output =
            serialize_manifest_yaml(&manifest).expect("advanced disk options should serialize");
        assert!(output.contains("discard: unmap"));
        assert!(output.contains("detectZeroes: on"));
        assert!(output.contains("readonly: false"));
        assert!(output.contains("serial: null"));
        assert!(output.contains("totalBytesPerSec: 10000000"));

        let remove = parse_manifest_yaml(&yaml.replace(
            "  ioTune:\n    totalBytesPerSec: 10000000\n    readIops: 400\n    writeIops: 100\n",
            "  ioTune: null\n",
        ))
        .expect("null ioTune should parse");
        assert_eq!(
            remove.disks[0].as_present().unwrap().io_tune,
            VmDiskIoTuneConfig::Remove
        );
    }

    #[test]
    fn rejects_invalid_advanced_disk_modes() {
        for field in ["discard: trim", "detectZeroes: auto"] {
            let yaml =
                format!("name: vm\ndisks:\n- path: /tmp/root.qcow2\n  format: qcow2\n  {field}\n");
            assert!(parse_manifest_yaml(&yaml).is_err(), "accepted {field}");
        }
    }

    #[test]
    fn rejects_disk_state_in_v1_and_present_fields_on_absent_disk() {
        let v1 = "schemaVersion: 1\nname: vm\ndisks:\n- id: data\n  state: absent\n";
        let invalid_absent = "schemaVersion: 2\nname: vm\ndisks:\n- id: data\n  state: absent\n  path: /tmp/data.qcow2\n  format: qcow2\n";
        let invalid_advanced_absent =
            "schemaVersion: 2\nname: vm\ndisks:\n- id: data\n  state: absent\n  readonly: true\n";

        assert!(
            parse_manifest_yaml(v1)
                .unwrap_err()
                .to_string()
                .contains("requires schemaVersion 2")
        );
        let error = parse_manifest_yaml(invalid_absent).unwrap_err();
        assert!(format!("{error:#}").contains("absent disk only accepts id and state"));
        let error = parse_manifest_yaml(invalid_advanced_absent).unwrap_err();
        assert!(format!("{error:#}").contains("absent disk only accepts id and state"));
    }

    #[test]
    fn parses_loaded_empty_and_absent_cdroms() {
        let yaml = r#"schemaVersion: 2
name: vm
disks:
- id: root
  path: /tmp/root.qcow2
  format: qcow2
cdroms:
- id: installer
  media: /isos/os.iso
- id: tools
  media: null
- id: retired
  state: absent
"#;

        let manifest = parse_manifest_yaml(yaml).expect("CD-ROM entries should parse");
        let output = serialize_manifest_yaml(&manifest).expect("CD-ROM entries should serialize");
        let cdroms = manifest.cdroms.as_ref().unwrap();

        assert_eq!(
            cdroms[0].as_present().unwrap().media,
            Some(PathBuf::from("/isos/os.iso"))
        );
        assert_eq!(cdroms[1].as_present().unwrap().media, None);
        assert_eq!(cdroms[2].absent_id(), Some("retired"));
        assert!(output.contains("media: null"));
        assert!(output.contains("state: absent"));
    }

    #[test]
    fn rejects_cdroms_in_v1_and_legacy_cdrom_conflict() {
        let v1 = "schemaVersion: 1\nname: vm\ndisks: []\ncdroms: []\n";
        let conflict = "schemaVersion: 2\nname: vm\ndisks: []\ncdrom: /isos/os.iso\ncdroms: []\n";

        assert!(
            parse_manifest_yaml(v1)
                .unwrap_err()
                .to_string()
                .contains("cdroms requires schemaVersion 2")
        );
        assert!(
            parse_manifest_yaml(conflict)
                .unwrap_err()
                .to_string()
                .contains("cdrom and cdroms are mutually exclusive")
        );
    }

    #[test]
    fn parses_and_serializes_structured_resources_without_legacy_fields() {
        let yaml = r#"schemaVersion: 1
name: install-os
machine:
  type: pc-q35-10.0
cpu:
  mode: custom
  model: EPYC-Milan
  topology:
    sockets: 2
    cores: 4
    threads: 2
memory:
  sizeMiB: 6144
  maxMiB: 8192
disks:
- path: /tmp/sys.qcow2
  format: qcow2
"#;

        let manifest = parse_manifest_yaml(yaml).expect("structured resources should parse");
        let output = serialize_manifest_yaml(&manifest).expect("manifest should serialize");

        assert_eq!(
            manifest.machine,
            Some(VmMachine {
                machine_type: "pc-q35-10.0".to_string(),
            })
        );
        assert_eq!(effective_vcpus(&manifest).unwrap(), 16);
        assert_eq!(effective_memory(&manifest).unwrap().size_mib, 6144);
        assert!(output.contains("mode: custom"));
        assert!(output.contains("sizeMiB: 6144"));
        assert!(!output.contains("memoryGiB"));
        assert!(!output.contains("\nvcpus:"));
    }

    #[test]
    fn rejects_mixed_legacy_and_structured_resources() {
        let mixed_memory = "name: vm\nmemoryGiB: 4\nmemory:\n  sizeMiB: 4096\ndisks: []\n";
        let mixed_cpu = "name: vm\nvcpus: 2\ncpu:\n  vcpus: 2\ndisks: []\n";

        assert!(
            parse_manifest_yaml(mixed_memory)
                .unwrap_err()
                .to_string()
                .contains("memory and memoryGiB are mutually exclusive")
        );
        assert!(
            parse_manifest_yaml(mixed_cpu)
                .unwrap_err()
                .to_string()
                .contains("cpu and vcpus are mutually exclusive")
        );
    }

    #[test]
    fn rejects_unsupported_manifest_schema_version() {
        let error = parse_manifest_yaml("schemaVersion: 3\nname: vm\ndisks: []\n")
            .expect_err("future schema should be rejected");

        assert!(error.to_string().contains("unsupported VM schemaVersion 3"));
    }

    #[test]
    fn parses_domain_capabilities() {
        let xml = r#"<domainCapabilities>
  <path>/usr/bin/qemu-system-x86_64</path>
  <domain>kvm</domain>
  <machine>pc-q35-10.0</machine>
  <arch>x86_64</arch>
  <vcpu max='288'/>
  <os supported='yes'>
    <enum name='firmware'>
      <value>bios</value>
      <value>efi</value>
    </enum>
  </os>
  <cpu>
    <mode name='host-passthrough' supported='yes'/>
    <mode name='host-model' supported='yes'/>
    <mode name='custom' supported='no'/>
  </cpu>
  <devices>
    <disk supported='yes'>
      <enum name='diskDevice'><value>disk</value><value>cdrom</value></enum>
      <enum name='bus'><value>scsi</value><value>virtio</value></enum>
    </disk>
    <tpm supported='yes'>
      <enum name='backendVersion'><value>2.0</value></enum>
    </tpm>
    <hostdev supported='no'/>
  </devices>
</domainCapabilities>"#;

        let capabilities = parse_domain_capabilities(xml).expect("capabilities should parse");

        assert_eq!(capabilities.domain_type, "kvm");
        assert_eq!(capabilities.architecture, "x86_64");
        assert_eq!(capabilities.machine.as_deref(), Some("pc-q35-10.0"));
        assert_eq!(capabilities.max_vcpus, Some(288));
        assert_eq!(capabilities.firmware, ["bios", "efi"]);
        assert_eq!(capabilities.cpu_modes, ["host-passthrough", "host-model"]);
        assert_eq!(capabilities.devices[0].options["bus"], ["scsi", "virtio"]);
        assert_eq!(capabilities.devices[1].device, "tpm");
    }

    #[test]
    fn colorizes_unified_diff_lines() {
        let diff = "--- old\n+++ new\n@@ -1 +1 @@\n-old\n+new\n same\n";

        let colored = colorize_unified_diff(diff);

        assert!(colored.contains("\x1b[31m--- old\x1b[0m\n"));
        assert!(colored.contains("\x1b[32m+++ new\x1b[0m\n"));
        assert!(colored.contains("\x1b[36m@@ -1 +1 @@\x1b[0m\n"));
        assert!(colored.contains("\x1b[31m-old\x1b[0m\n"));
        assert!(colored.contains("\x1b[32m+new\x1b[0m\n"));
        assert!(colored.contains(" same\n"));
    }

    fn test_domain_xml() -> &'static str {
        r#"<domain type='kvm'>
  <name>install-os</name>
  <memory unit='MiB'>4096</memory>
  <currentMemory unit='MiB'>4096</currentMemory>
  <vcpu placement='static'>2</vcpu>
  <os>
    <type arch='x86_64'>hvm</type>
    <boot dev='hd'/>
  </os>
  <devices>
    <disk type='file' device='disk'>
      <driver name='qemu' type='qcow2'/>
      <source file='/vm/sys.qcow2'/>
      <target dev='vda' bus='virtio'/>
      <address type='pci' domain='0x0000' bus='0x00' slot='0x07' function='0x0'/>
    </disk>
    <interface type='network'>
      <source network='default'/>
      <model type='virtio'/>
    </interface>
  </devices>
</domain>
"#
    }

    fn test_manifest(disks: Vec<VmDisk>) -> VmManifest {
        VmManifest {
            name: "install-os".to_string(),
            machine: None,
            cpu: None,
            memory: None,
            io_threads: None,
            disks: disks.into_iter().map(VmDiskEntry::present).collect(),
            cdrom: None,
            cdroms: None,
            boot: Some(vec!["hd".to_string()]),
            memory_gib: 4,
            vcpus: 2,
            network: "default".to_string(),
            graphics: GraphicsMode::None,
            vnc_listen: "127.0.0.1".to_string(),
            vnc_port: None,
            serial_log: None,
        }
    }

    fn test_file_disk(path: &str, target: Option<&str>, bus: VmDiskBus) -> VmDisk {
        VmDisk {
            id: None,
            disk_type: VmDiskType::File,
            path: PathBuf::from(path),
            format: DiskFormat::Qcow2,
            target: target.map(str::to_string),
            bus,
            cache: None,
            io: None,
            discard: None,
            detect_zeroes: None,
            readonly: None,
            serial: VmDiskSerial::default(),
            io_tune: VmDiskIoTuneConfig::default(),
        }
    }

    struct TestDiskDir(PathBuf);

    impl TestDiskDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("qtr-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).expect("failed to create temp test dir");
            Self(dir)
        }

        fn create_disk(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, b"").expect("failed to create temp disk file");
            path
        }
    }

    impl Drop for TestDiskDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn validate_manifest_accepts_valid_manifest() {
        let dir = TestDiskDir::new();
        let disk = dir.create_disk("sys.qcow2");
        let manifest = test_manifest(vec![test_file_disk(
            disk.to_str().unwrap(),
            Some("vda"),
            VmDiskBus::VirtioBlk,
        )]);
        assert!(validate_manifest(&manifest).is_ok());
    }

    #[test]
    fn validate_manifest_accepts_structured_resources() {
        let dir = TestDiskDir::new();
        let disk = dir.create_disk("sys.qcow2");
        let mut manifest = test_manifest(vec![test_file_disk(
            disk.to_str().unwrap(),
            Some("vda"),
            VmDiskBus::VirtioBlk,
        )]);
        manifest.machine = Some(VmMachine {
            machine_type: "q35".to_string(),
        });
        manifest.cpu = Some(VmCpu {
            mode: VmCpuMode::Custom,
            model: Some("EPYC-Milan".to_string()),
            vcpus: None,
            topology: Some(VmCpuTopology {
                sockets: 1,
                cores: 4,
                threads: 2,
            }),
        });
        manifest.memory = Some(VmMemory {
            size_mib: 6144,
            max_mib: Some(8192),
        });

        assert!(validate_manifest(&manifest).is_ok());
    }

    #[test]
    fn validate_manifest_rejects_invalid_structured_resources() {
        let dir = TestDiskDir::new();
        let disk = dir.create_disk("sys.qcow2");
        let mut manifest = test_manifest(vec![test_file_disk(
            disk.to_str().unwrap(),
            Some("vda"),
            VmDiskBus::VirtioBlk,
        )]);
        manifest.memory = Some(VmMemory {
            size_mib: 4096,
            max_mib: Some(2048),
        });
        assert!(
            validate_manifest(&manifest)
                .unwrap_err()
                .to_string()
                .contains("memory.maxMiB")
        );

        manifest.memory = None;
        manifest.cpu = Some(VmCpu {
            mode: VmCpuMode::Custom,
            model: None,
            vcpus: Some(2),
            topology: None,
        });
        assert!(
            validate_manifest(&manifest)
                .unwrap_err()
                .to_string()
                .contains("requires cpu.model")
        );

        manifest.cpu = Some(VmCpu {
            mode: VmCpuMode::HostModel,
            model: None,
            vcpus: Some(2),
            topology: Some(VmCpuTopology {
                sockets: 1,
                cores: 2,
                threads: 1,
            }),
        });
        assert!(
            validate_manifest(&manifest)
                .unwrap_err()
                .to_string()
                .contains("mutually exclusive")
        );

        manifest.cpu = None;
        manifest.disks[0].as_present_mut().unwrap().serial = VmDiskSerial::value("   ");
        assert!(
            validate_manifest(&manifest)
                .unwrap_err()
                .to_string()
                .contains("disk serial must not be empty")
        );
    }

    #[test]
    fn validate_manifest_rejects_invalid_disk_iotune() {
        let dir = TestDiskDir::new();
        let path = dir.create_disk("sys.qcow2");
        let mut manifest = test_manifest(vec![test_file_disk(
            path.to_str().unwrap(),
            Some("vda"),
            VmDiskBus::VirtioBlk,
        )]);
        let disk = manifest.disks[0].as_present_mut().unwrap();
        disk.io_tune = VmDiskIoTuneConfig::configured(VmDiskIoTune {
            total_bytes_per_sec: None,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
            total_iops: None,
            read_iops: None,
            write_iops: None,
        });
        assert!(
            validate_manifest(&manifest)
                .unwrap_err()
                .to_string()
                .contains("at least one limit")
        );

        manifest.disks[0].as_present_mut().unwrap().io_tune =
            VmDiskIoTuneConfig::configured(VmDiskIoTune {
                total_bytes_per_sec: Some(1_000_000),
                read_bytes_per_sec: Some(750_000),
                write_bytes_per_sec: None,
                total_iops: Some(0),
                read_iops: Some(100),
                write_iops: None,
            });
        assert!(
            validate_manifest(&manifest)
                .unwrap_err()
                .to_string()
                .contains("totalBytesPerSec cannot be combined")
        );

        manifest.disks[0].as_present_mut().unwrap().io_tune =
            VmDiskIoTuneConfig::configured(VmDiskIoTune {
                total_bytes_per_sec: None,
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
                total_iops: Some(1_000),
                read_iops: Some(100),
                write_iops: None,
            });
        assert!(
            validate_manifest(&manifest)
                .unwrap_err()
                .to_string()
                .contains("totalIops cannot be combined")
        );
    }

    #[test]
    fn validate_manifest_rejects_empty_disks() {
        let manifest = test_manifest(vec![]);
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(err.to_string().contains("at least one disk"));
    }

    #[test]
    fn validate_manifest_rejects_zero_iothreads() {
        let dir = TestDiskDir::new();
        let disk = dir.create_disk("sys.qcow2");

        let mut manifest = test_manifest(vec![test_file_disk(
            disk.to_str().unwrap(),
            None,
            VmDiskBus::VirtioBlk,
        )]);
        manifest.io_threads = Some(VmIoThreads {
            count: 0,
            queues: None,
        });
        assert!(validate_manifest(&manifest).is_err());

        let mut manifest = test_manifest(vec![test_file_disk(
            disk.to_str().unwrap(),
            None,
            VmDiskBus::VirtioBlk,
        )]);
        manifest.io_threads = Some(VmIoThreads {
            count: 1,
            queues: Some(0),
        });
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn validate_manifest_requires_iothreads_pairing() {
        let dir = TestDiskDir::new();
        let disk = dir.create_disk("sys.qcow2");

        let mut threads_disk = test_file_disk(disk.to_str().unwrap(), None, VmDiskBus::VirtioBlk);
        threads_disk.io = Some(VmDiskIoConfig {
            mode: VmDiskIoMode::Threads,
        });
        let manifest = test_manifest(vec![threads_disk]);
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(err.to_string().contains("requires ioThreads"));

        let mut manifest = test_manifest(vec![test_file_disk(
            disk.to_str().unwrap(),
            None,
            VmDiskBus::VirtioBlk,
        )]);
        manifest.io_threads = Some(VmIoThreads {
            count: 2,
            queues: None,
        });
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(err.to_string().contains("io.mode threads"));
    }

    #[test]
    fn validate_manifest_rejects_duplicate_targets() {
        let dir = TestDiskDir::new();
        let first = dir.create_disk("a.qcow2");
        let second = dir.create_disk("b.qcow2");
        let manifest = test_manifest(vec![
            test_file_disk(first.to_str().unwrap(), Some("vda"), VmDiskBus::VirtioBlk),
            test_file_disk(second.to_str().unwrap(), Some("vda"), VmDiskBus::VirtioBlk),
        ]);
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(err.to_string().contains("duplicate disk target vda"));
    }

    #[test]
    fn validate_manifest_rejects_invalid_and_duplicate_disk_ids() {
        let dir = TestDiskDir::new();
        let first = dir.create_disk("a.qcow2");
        let second = dir.create_disk("b.qcow2");
        let mut first_disk =
            test_file_disk(first.to_str().unwrap(), Some("vda"), VmDiskBus::VirtioBlk);
        let mut second_disk =
            test_file_disk(second.to_str().unwrap(), Some("vdb"), VmDiskBus::VirtioBlk);
        first_disk.id = Some("root".to_string());
        second_disk.id = Some("root".to_string());
        let manifest = test_manifest(vec![first_disk.clone(), second_disk]);
        assert!(
            validate_manifest(&manifest)
                .unwrap_err()
                .to_string()
                .contains("duplicate disk id root")
        );

        first_disk.id = Some("invalid/id".to_string());
        let manifest = test_manifest(vec![first_disk]);
        assert!(
            validate_manifest(&manifest)
                .unwrap_err()
                .to_string()
                .contains("unsupported characters")
        );
    }

    #[test]
    fn validate_manifest_rejects_mismatched_target_prefix() {
        let dir = TestDiskDir::new();
        let blk_disk = dir.create_disk("blk.qcow2");
        let manifest = test_manifest(vec![test_file_disk(
            blk_disk.to_str().unwrap(),
            Some("sda"),
            VmDiskBus::VirtioBlk,
        )]);
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(err.to_string().contains("must start with vd"));

        let scsi_disk = dir.create_disk("scsi.qcow2");
        let manifest = test_manifest(vec![test_file_disk(
            scsi_disk.to_str().unwrap(),
            Some("vda"),
            VmDiskBus::VirtioScsi,
        )]);
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(err.to_string().contains("must start with sd"));
    }

    #[test]
    fn validate_manifest_rejects_missing_disk_path() {
        let manifest = test_manifest(vec![test_file_disk(
            "/nonexistent/qtr-missing-disk.qcow2",
            None,
            VmDiskBus::VirtioBlk,
        )]);
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn validate_manifest_rejects_relative_block_disk_path() {
        let name = format!("qtr-test-{}.raw", uuid::Uuid::new_v4());
        fs::write(&name, b"").expect("failed to create relative test file");
        let mut disk = test_file_disk(&name, None, VmDiskBus::VirtioScsi);
        disk.disk_type = VmDiskType::Block;
        disk.format = DiskFormat::Raw;
        let manifest = test_manifest(vec![disk]);
        let err = validate_manifest(&manifest).unwrap_err();
        let _ = fs::remove_file(&name);
        assert!(err.to_string().contains("absolute path"));
    }

    #[test]
    fn join_command_args_quotes_each_argument() {
        let args = vec!["touch".to_string(), "a b".to_string()];
        assert_eq!(join_command_args(&args), "'touch' 'a b'");

        let args = vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo '$HOME' | head".to_string(),
        ];
        assert_eq!(
            join_command_args(&args),
            "'sh' '-c' 'echo '\\''$HOME'\\'' | head'"
        );

        let args = vec!["uname".to_string(), "-a".to_string()];
        assert_eq!(join_command_args(&args), "'uname' '-a'");
    }

    #[test]
    fn domain_states_match_the_web_contract() {
        let expected: Vec<String> =
            serde_json::from_str(include_str!("../fixtures/vm-states.json"))
                .expect("VM state fixture should parse");
        let states = [
            sys::VIR_DOMAIN_NOSTATE,
            sys::VIR_DOMAIN_RUNNING,
            sys::VIR_DOMAIN_BLOCKED,
            sys::VIR_DOMAIN_PAUSED,
            sys::VIR_DOMAIN_SHUTDOWN,
            sys::VIR_DOMAIN_SHUTOFF,
            sys::VIR_DOMAIN_CRASHED,
            sys::VIR_DOMAIN_PMSUSPENDED,
            u32::MAX,
        ]
        .map(domain_state_name);

        assert_eq!(states.as_slice(), expected);
    }

    #[test]
    fn lifecycle_state_actions_are_idempotent_and_reject_inactive_states() {
        assert_eq!(suspend_state_action(sys::VIR_DOMAIN_RUNNING), Some(true));
        assert_eq!(suspend_state_action(sys::VIR_DOMAIN_BLOCKED), Some(true));
        assert_eq!(suspend_state_action(sys::VIR_DOMAIN_PAUSED), Some(false));
        assert_eq!(suspend_state_action(sys::VIR_DOMAIN_SHUTOFF), None);

        assert_eq!(resume_state_action(sys::VIR_DOMAIN_PAUSED), Some(true));
        assert_eq!(resume_state_action(sys::VIR_DOMAIN_RUNNING), Some(false));
        assert_eq!(resume_state_action(sys::VIR_DOMAIN_BLOCKED), Some(false));
        assert_eq!(resume_state_action(sys::VIR_DOMAIN_SHUTOFF), None);
    }

    #[test]
    fn libvirt_test_driver_supports_managed_save_lifecycle() {
        let conn = Connect::open(Some("test:///default")).expect("test driver should connect");
        let domain = Domain::lookup_by_name(&conn, "test").expect("test domain should exist");

        domain.managed_save(0).expect("managed save should succeed");
        assert!(
            !domain
                .is_active()
                .expect("domain state should be available")
        );
        assert!(
            domain
                .has_managed_save(0)
                .expect("saved state should be available")
        );

        domain.create().expect("managed restore should succeed");
        assert!(
            domain
                .is_active()
                .expect("domain state should be available")
        );
        assert!(
            !domain
                .has_managed_save(0)
                .expect("saved state should be consumed")
        );

        domain.managed_save(0).expect("second save should succeed");
        domain
            .managed_save_remove(0)
            .expect("saved state removal should succeed");
        assert!(
            !domain
                .has_managed_save(0)
                .expect("saved state should be removed")
        );
    }

    #[test]
    fn wait_for_shutdown_returns_when_inactive() {
        assert!(wait_for_shutdown(|| Ok(false), "test", None).is_ok());
    }

    #[test]
    fn wait_for_shutdown_times_out_while_active() {
        let err = wait_for_shutdown(|| Ok(true), "test", Some(Duration::ZERO)).unwrap_err();
        assert!(err.to_string().contains("timed out waiting for VM test"));
    }

    #[test]
    fn wait_for_shutdown_propagates_query_errors() {
        let err = wait_for_shutdown(|| bail!("query failed"), "test", None).unwrap_err();
        assert!(err.to_string().contains("query failed"));
    }

    #[test]
    fn validate_manifest_rejects_missing_cdrom() {
        let dir = TestDiskDir::new();
        let disk = dir.create_disk("sys.qcow2");
        let mut manifest = test_manifest(vec![test_file_disk(
            disk.to_str().unwrap(),
            None,
            VmDiskBus::VirtioBlk,
        )]);
        manifest.cdrom = Some(PathBuf::from("/nonexistent/qtr-missing.iso"));
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }
}
