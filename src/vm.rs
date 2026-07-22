use std::{
    collections::BTreeSet,
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
use serde::{Deserialize, Deserializer, Serialize};
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
        ColorMode, DiskFormat, GraphicsMode, VmApplyArgs, VmArgs, VmCommand, VmCpArgs, VmDumpArgs,
        VmExecArgs, VmInitArgs, VmListArgs, VmNameArgs, VmRemoveArgs, VmStartArgs, VmStopArgs,
    },
    domain_xml::{
        self, BootDevice, GraphicsSpec, VmLaunchDiskSource, VmLaunchDiskSpec, VmLaunchDomainSpec,
        VmLaunchIoThreadsSpec, build_vm_launch_domain_xml, parse_boot_devices,
    },
    guest_agent,
};

pub fn run(args: VmArgs) -> Result<()> {
    clear_error_callback();

    match args.command {
        VmCommand::Init(args) => init(args),
        VmCommand::Apply(args) => apply(args),
        VmCommand::Dump(args) => dump(args),
        VmCommand::List(args) => list(args),
        VmCommand::Start(args) => start(args),
        VmCommand::Stop(args) => stop(args),
        VmCommand::Rm(args) => remove(args),
        VmCommand::Vnc(args) => vnc(args),
        VmCommand::Exec(args) => exec(args),
        VmCommand::Cp(args) => cp(args),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmManifest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_threads: Option<VmIoThreads>,
    pub disks: Vec<VmDisk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cdrom: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot: Option<Vec<String>>,
    #[serde(default = "default_vm_memory_gib", rename = "memoryGiB")]
    pub memory_gib: u64,
    #[serde(default = "default_vm_vcpus")]
    pub vcpus: u32,
    #[serde(default = "default_vm_network")]
    pub network: String,
    #[serde(default = "default_vm_graphics")]
    pub graphics: GraphicsMode,
    #[serde(default = "default_vm_vnc_listen")]
    pub vnc_listen: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vnc_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_log: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmDisk {
    #[serde(default = "default_vm_disk_type", rename = "type")]
    pub disk_type: VmDiskType,
    pub path: PathBuf,
    pub format: DiskFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default)]
    pub bus: VmDiskBus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<VmDiskCache>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io: Option<VmDiskIoConfig>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmIoThreads {
    pub count: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queues: Option<u16>,
}

impl VmIoThreads {
    fn effective(self) -> VmLaunchIoThreadsSpec {
        VmLaunchIoThreadsSpec {
            count: self.count,
            queues: self.queues.unwrap_or(self.count),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VmDiskType {
    File,
    Block,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VmDiskCache {
    Default,
    None,
    Writethrough,
    Writeback,
    Directsync,
    Unsafe,
}

impl VmDiskCache {
    fn as_xml(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::None => "none",
            Self::Writethrough => "writethrough",
            Self::Writeback => "writeback",
            Self::Directsync => "directsync",
            Self::Unsafe => "unsafe",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmDiskIoConfig {
    pub mode: VmDiskIoMode,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VmDiskIoMode {
    Threads,
    Native,
    IoUring,
}

impl VmDiskIoMode {
    fn as_xml(self) -> &'static str {
        match self {
            Self::Threads => "threads",
            Self::Native => "native",
            Self::IoUring => "io_uring",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VmDiskBus {
    #[default]
    VirtioBlk,
    VirtioScsi,
}

impl<'de> Deserialize<'de> for VmDiskBus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "virtio" | "virtio-blk" => Ok(Self::VirtioBlk),
            "virtio-scsi" => Ok(Self::VirtioScsi),
            _ => Err(serde::de::Error::unknown_variant(
                &value,
                &["virtio-blk", "virtio-scsi"],
            )),
        }
    }
}

impl VmDiskBus {
    fn target_bus(self) -> &'static str {
        match self {
            Self::VirtioBlk => "virtio",
            Self::VirtioScsi => "scsi",
        }
    }
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

fn default_vm_memory_gib() -> u64 {
    4
}

fn default_vm_vcpus() -> u32 {
    2
}

fn default_vm_network() -> String {
    "default".to_string()
}

fn default_vm_graphics() -> GraphicsMode {
    GraphicsMode::Vnc
}

fn default_vm_vnc_listen() -> String {
    "127.0.0.1".to_string()
}

fn default_vm_disk_type() -> VmDiskType {
    VmDiskType::File
}

fn init(args: VmInitArgs) -> Result<()> {
    let disk_paths = if args.disk.is_empty() {
        vec![PathBuf::from(format!(".tmp/disks/{}.qcow2", args.name))]
    } else {
        args.disk
    };
    let disks = disk_paths
        .into_iter()
        .map(|path| VmDisk {
            disk_type: VmDiskType::File,
            path,
            format: DiskFormat::Qcow2,
            target: None,
            bus: VmDiskBus::VirtioBlk,
            cache: None,
            io: None,
        })
        .collect();
    let boot = if args.no_cdrom {
        vec!["hd".to_string()]
    } else {
        vec!["cdrom".to_string(), "hd".to_string()]
    };

    let manifest = VmManifest {
        name: args.name,
        io_threads: None,
        disks,
        cdrom: (!args.no_cdrom).then_some(args.cdrom),
        boot: Some(boot),
        memory_gib: args.memory_gib,
        vcpus: args.vcpus,
        network: args.network,
        graphics: GraphicsMode::Vnc,
        vnc_listen: args.vnc_listen,
        vnc_port: None,
        serial_log: None,
    };

    let yaml = serde_yaml::to_string(&manifest).context("failed to serialize VM template")?;
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
    let mut manifest: VmManifest = serde_yaml::from_str(&manifest_text)
        .with_context(|| format!("failed to parse VM definition {}", manifest_path.display()))?;

    normalize_manifest_paths(&mut manifest, manifest_dir)?;
    validate_manifest(&manifest)?;

    let boot = manifest_boot_order(&manifest);
    let boot_devices = domain_xml::parse_boot_devices(&boot)?;
    if boot_devices.contains(&BootDevice::Cdrom) && manifest.cdrom.is_none() {
        bail!("boot order contains cdrom but cdrom was not provided");
    }

    let memory_mib = manifest
        .memory_gib
        .checked_mul(1024)
        .context("memoryGiB is too large")?;

    let current_xml = current_domain_xml(&args.connect_uri, &manifest.name)?;
    let xml = if current_xml.is_empty() {
        build_manifest_domain_xml(&manifest, &boot_devices, memory_mib)?
    } else {
        patch_domain_xml(&current_xml, &manifest, &boot_devices, memory_mib)?
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

fn build_manifest_domain_xml(
    manifest: &VmManifest,
    boot_devices: &[BootDevice],
    memory_mib: u64,
) -> Result<String> {
    let targets = assign_manifest_disk_targets(manifest)?;
    let disks = manifest
        .disks
        .iter()
        .zip(targets)
        .map(|(disk, target)| launch_disk_spec(disk, target, manifest.io_threads))
        .collect::<Vec<_>>();

    Ok(build_vm_launch_domain_xml(VmLaunchDomainSpec {
        name: &manifest.name,
        memory_mib,
        vcpus: manifest.vcpus,
        io_threads: manifest.io_threads.map(VmIoThreads::effective),
        disks: &disks,
        cdrom: manifest.cdrom.as_deref(),
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
        path: disk.path.clone(),
        format: disk.format,
        source: disk_launch_source(disk.disk_type),
        target,
        bus: disk.bus.target_bus().to_string(),
        cache: disk.cache.map(VmDiskCache::as_xml),
        io: disk.io.map(|io| io.mode.as_xml()),
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
    let mut occupied = BTreeSet::new();
    if manifest.cdrom.is_some() {
        occupied.insert(domain_xml::CDROM_TARGET.to_string());
    }

    manifest
        .disks
        .iter()
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

fn dump(args: VmDumpArgs) -> Result<()> {
    let xml = existing_domain_xml(&args.connect_uri, &args.name)?;
    let output = if args.xml {
        xml
    } else {
        let manifest = manifest_from_domain_xml(&xml)?;
        serde_yaml::to_string(&manifest)
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
    let memory_mib = memory_mib(domain)?;
    if memory_mib % 1024 != 0 {
        bail!("domain memory {memory_mib} MiB cannot be represented as whole GiB");
    }

    let boot = boot_order(domain)?;
    let devices = required_child(domain, "devices")?;
    let disks = disks_from_domain_xml(devices)?;
    let io_threads = io_threads_from_domain_xml(domain, devices)?;
    let cdrom = optional_disk_source_path(devices, "cdrom", None)?;
    let network = network_name(devices)?;
    let (graphics, vnc_listen, vnc_port) = graphics_config(devices)?;
    let serial_log = serial_log_path(devices);

    Ok(VmManifest {
        name,
        io_threads,
        disks,
        cdrom,
        boot: Some(boot),
        memory_gib: memory_mib / 1024,
        vcpus: required_child_text(domain, "vcpu")?
            .parse()
            .context("failed to parse domain vcpus")?,
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
    memory_mib: u64,
) -> Result<String> {
    let doc = Document::parse(xml).context("failed to parse existing libvirt domain XML")?;
    let domain = doc.root_element();
    let devices = required_child(domain, "devices")?;
    let mut replacements = Vec::new();

    patch_memory(xml, domain, memory_mib, &mut replacements)?;
    patch_io_threads(xml, domain, manifest.io_threads, &mut replacements)?;
    push_text_replacement(
        xml,
        required_child(domain, "vcpu")?,
        &manifest.vcpus.to_string(),
        &mut replacements,
    )?;
    patch_boot_order(xml, domain, boot_devices, &mut replacements)?;
    patch_disks(
        xml,
        devices,
        &manifest.disks,
        manifest.io_threads,
        &mut replacements,
    )?;
    patch_virtio_scsi_controller(
        xml,
        devices,
        &manifest.disks,
        manifest.io_threads,
        &mut replacements,
    );

    if let Some(cdrom) = &manifest.cdrom {
        patch_disk_source(xml, devices, "cdrom", None, cdrom, &mut replacements)?;
    }

    patch_network(xml, devices, &manifest.network, &mut replacements)?;
    patch_graphics(xml, devices, manifest, &mut replacements)?;

    if let Some(serial_log) = &manifest.serial_log {
        patch_serial_log(xml, devices, serial_log, &mut replacements)?;
    }

    Ok(apply_xml_replacements(xml, replacements))
}

fn patch_disks(
    xml: &str,
    devices: Node<'_, '_>,
    manifest_disks: &[VmDisk],
    io_threads: Option<VmIoThreads>,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let domain_disk_nodes = devices
        .children()
        .filter(|child| child.has_tag_name("disk") && child.attribute("device") == Some("disk"))
        .collect::<Vec<_>>();
    let domain_disks = disks_from_domain_xml(devices)?;
    if manifest_disks.len() < domain_disks.len() {
        bail!(
            "cannot remove existing domain XML disks from {} to {}; recreate the VM definition",
            domain_disks.len(),
            manifest_disks.len()
        );
    }

    validate_implicit_disk_identities(manifest_disks)?;

    let mut domain_disks = domain_disk_nodes
        .into_iter()
        .zip(domain_disks)
        .map(|(node, disk)| DomainDiskPatchEntry {
            node,
            disk,
            used: false,
        })
        .collect::<Vec<_>>();
    let mut occupied_targets = domain_disks
        .iter()
        .filter_map(|entry| entry.disk.target.clone())
        .collect::<BTreeSet<_>>();
    occupied_targets.extend(
        devices
            .children()
            .filter(|child| {
                child.has_tag_name("disk") && child.attribute("device") == Some("cdrom")
            })
            .filter_map(|node| disk_target_dev(node)),
    );
    let mut output_targets = BTreeSet::new();
    let mut new_disks_xml = String::new();

    for (index, manifest_disk) in manifest_disks.iter().enumerate() {
        let matched_index = find_domain_disk_match(&domain_disks, manifest_disk)?;
        let target = disk_patch_target(
            manifest_disk,
            matched_index.and_then(|index| domain_disks[index].disk.target.as_deref()),
            &mut occupied_targets,
            &output_targets,
        )?;
        reserve_output_disk_target(&target, &mut output_targets)?;

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

    let needs_scsi_controller = manifest_disks
        .iter()
        .any(|disk| disk.bus == VmDiskBus::VirtioScsi)
        && !has_virtio_scsi_controller(devices);
    if needs_scsi_controller {
        new_disks_xml.push_str(&domain_xml::build_virtio_scsi_controller_xml(
            virtio_scsi_controller_io_threads(manifest_disks, io_threads),
        ));
    }

    if !new_disks_xml.is_empty() {
        let last_disk = domain_disks
            .iter()
            .map(|entry| entry.node)
            .next_back()
            .context("cannot append disks because existing domain XML has no disk devices")?;
        let range = last_disk.range();
        let end = line_end(xml, range.end);
        replacements.push(XmlReplacement {
            range: end..end,
            value: new_disks_xml,
        });
    }

    Ok(())
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

fn disk_patch_target(
    disk: &VmDisk,
    existing_target: Option<&str>,
    occupied_targets: &mut BTreeSet<String>,
    output_targets: &BTreeSet<String>,
) -> Result<String> {
    let target = if let Some(target) = disk.target.as_deref() {
        if existing_target != Some(target) && occupied_targets.contains(target) {
            bail!("disk target {target} is already used by an existing disk");
        }
        occupied_targets.insert(target.to_string());
        target.to_string()
    } else if let Some(target) = existing_target {
        target.to_string()
    } else {
        next_available_disk_target(disk.bus, occupied_targets, output_targets)
    };

    validate_disk_target_for_bus(&target, disk.bus)?;
    Ok(target)
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

fn reserve_output_disk_target(target: &str, output_targets: &mut BTreeSet<String>) -> Result<()> {
    if !output_targets.insert(target.to_string()) {
        bail!("duplicate disk target {target}");
    }

    Ok(())
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
    let mut desired = domain_xml::build_disk_xml(&launch_disk_spec(
        manifest_disk,
        disk_target_or(manifest_disk, index),
        io_threads,
    ));
    let addresses = domain_disk
        .children()
        .filter(|child| child.has_tag_name("address"))
        .map(|address| {
            let range = address.range();
            let start = line_start(xml, range.start);
            let end = line_end(xml, range.end);
            &xml[start..end]
        })
        .collect::<String>();

    if !addresses.is_empty()
        && let Some(pos) = desired.rfind("    </disk>\n")
    {
        desired.insert_str(pos, &addresses);
    }

    desired
}

fn patch_memory(
    xml: &str,
    domain: Node<'_, '_>,
    memory_mib: u64,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    for tag in ["memory", "currentMemory"] {
        let node = required_child(domain, tag)?;
        let unit = node.attribute("unit").unwrap_or("KiB");
        let value = memory_value_for_unit(memory_mib, unit)?;
        push_text_replacement(xml, node, &value.to_string(), replacements)?;
    }

    Ok(())
}

fn memory_value_for_unit(memory_mib: u64, unit: &str) -> Result<u64> {
    match unit {
        "KiB" => memory_mib
            .checked_mul(1024)
            .context("memoryGiB is too large for KiB domain memory"),
        "MiB" => Ok(memory_mib),
        "GiB" => {
            if !memory_mib.is_multiple_of(1024) {
                bail!("memoryGiB cannot be represented as whole GiB in existing domain XML");
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

fn node_text_range(node: Node<'_, '_>) -> Result<Range<usize>> {
    node.children()
        .find(|child| child.is_text())
        .map(|child| child.range())
        .with_context(|| format!("domain XML <{}> is missing text", node.tag_name().name()))
}

fn apply_xml_replacements(xml: &str, mut replacements: Vec<XmlReplacement>) -> String {
    replacements.sort_by_key(|replacement| replacement.range.start);

    let mut output = xml.to_string();
    for replacement in replacements.into_iter().rev() {
        output.replace_range(replacement.range, &replacement.value);
    }

    output
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

fn memory_mib(domain: Node<'_, '_>) -> Result<u64> {
    let memory = required_child(domain, "memory")?;
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

            Ok(VmDisk {
                disk_type,
                path: PathBuf::from(path),
                format,
                target: Some(target_dev),
                bus,
                cache,
                io,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    if disks.is_empty() {
        bail!("domain XML is missing disk devices");
    }

    Ok(disks)
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
    for disk in &mut manifest.disks {
        if disk.disk_type == VmDiskType::File {
            disk.path = manifest_relative_path(base_dir, &disk.path);
        }
    }

    if let Some(cdrom) = &manifest.cdrom {
        manifest.cdrom = Some(manifest_relative_path(base_dir, cdrom));
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
        .any(|disk| disk.io.map(|io| io.mode) == Some(VmDiskIoMode::Threads));
    if uses_io_threads && manifest.io_threads.is_none() {
        bail!("io.mode threads requires ioThreads");
    }
    if !uses_io_threads && manifest.io_threads.is_some() {
        bail!("ioThreads requires at least one disk with io.mode threads");
    }

    let targets = assign_manifest_disk_targets(manifest)?;
    for (disk, target) in manifest.disks.iter().zip(&targets) {
        validate_disk_target_for_bus(target, disk.bus)?;
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

    if let Some(cdrom) = &manifest.cdrom
        && !cdrom.exists()
    {
        bail!("cdrom ISO {} does not exist", cdrom.display());
    }

    Ok(())
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

pub fn get_summary(connect_uri: &str, name: &str) -> Result<VmSummary> {
    let conn = connect_read_only(connect_uri)?;
    let domain = lookup_domain(&conn, name)?;
    domain_summary(&domain)
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
                create_guest_parent_dir(&domain, &dest)?;
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

fn create_guest_parent_dir(domain: &Domain, path: &str) -> Result<()> {
    let Some(parent) = Path::new(path)
        .parent()
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty())
    else {
        return Ok(());
    };

    let command = format!("mkdir -p {}", shell_quote(&parent));
    let result = guest_agent::run_command(domain, &command, Duration::from_secs(30))
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
    let command = format!("kill -TERM -{pid} 2>/dev/null; kill -TERM {pid} 2>/dev/null");
    if let Err(err) = guest_agent::run_command(domain, &command, Duration::from_secs(10)) {
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

pub fn start_by_name(connect_uri: &str, name: &str) -> Result<()> {
    let conn = connect(connect_uri)?;
    let domain = lookup_domain(&conn, name)?;
    start_domain(&domain, name)
}

pub fn shutdown_by_name(connect_uri: &str, name: &str, wait: bool) -> Result<()> {
    let conn = connect(connect_uri)?;
    let domain = lookup_domain(&conn, name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {name} state"))?
    {
        return Ok(());
    }

    domain
        .shutdown()
        .with_context(|| format!("failed to request shutdown for domain {name}"))?;
    if wait {
        wait_shutdown_domain(&domain, name, None)?;
    }

    Ok(())
}

pub fn destroy_by_name(connect_uri: &str, name: &str) -> Result<()> {
    let conn = connect(connect_uri)?;
    let domain = lookup_domain(&conn, name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {name} state"))?
    {
        return Ok(());
    }

    domain
        .destroy()
        .with_context(|| format!("failed to destroy domain {name}"))
}

pub fn undefine_by_name(connect_uri: &str, name: &str) -> Result<()> {
    let conn = connect(connect_uri)?;
    let domain = lookup_domain(&conn, name)?;
    if domain
        .is_active()
        .with_context(|| format!("failed to query domain {name} state"))?
    {
        bail!("domain {name} is active; shutdown or destroy it first");
    }

    domain
        .undefine()
        .with_context(|| format!("failed to undefine domain {name}"))
}

pub fn create_by_manifest(connect_uri: &str, mut manifest: VmManifest) -> Result<VmSummary> {
    let base_dir = env::current_dir().context("failed to determine current directory")?;
    normalize_manifest_paths(&mut manifest, &base_dir)?;
    validate_manifest(&manifest)?;

    let boot = manifest_boot_order(&manifest);
    let boot_devices = parse_boot_devices(&boot)?;
    if boot_devices.contains(&BootDevice::Cdrom) && manifest.cdrom.is_none() {
        bail!("boot order contains cdrom but cdrom was not provided");
    }

    let memory_mib = manifest
        .memory_gib
        .checked_mul(1024)
        .context("memoryGiB is too large")?;
    let xml = build_manifest_domain_xml(&manifest, &boot_devices, memory_mib)?;

    prepare_serial_log_path(manifest.serial_log.as_deref())?;

    let conn = connect(connect_uri)?;
    let domain = Domain::define_xml(&conn, &xml)
        .with_context(|| format!("failed to define domain {}", manifest.name))?;

    domain_summary(&domain)
}

pub fn apply_by_manifest(connect_uri: &str, mut manifest: VmManifest) -> Result<VmSummary> {
    let base_dir = env::current_dir().context("failed to determine current directory")?;
    normalize_manifest_paths(&mut manifest, &base_dir)?;
    validate_manifest(&manifest)?;

    let boot = manifest_boot_order(&manifest);
    let boot_devices = parse_boot_devices(&boot)?;
    if boot_devices.contains(&BootDevice::Cdrom) && manifest.cdrom.is_none() {
        bail!("boot order contains cdrom but cdrom was not provided");
    }

    let memory_mib = manifest
        .memory_gib
        .checked_mul(1024)
        .context("memoryGiB is too large")?;

    let current_xml = current_domain_xml(connect_uri, &manifest.name)?;
    let xml = if current_xml.is_empty() {
        build_manifest_domain_xml(&manifest, &boot_devices, memory_mib)?
    } else {
        patch_domain_xml(&current_xml, &manifest, &boot_devices, memory_mib)?
    };

    prepare_serial_log_path(manifest.serial_log.as_deref())?;

    let conn = connect(connect_uri)?;
    let domain = Domain::define_xml_flags(&conn, &xml, sys::VIR_DOMAIN_DEFINE_VALIDATE)
        .with_context(|| format!("failed to apply VM definition {}", manifest.name))?;

    domain_summary(&domain)
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

    #[test]
    fn dumps_qtr_domain_xml_to_manifest() {
        let boot_devices = [BootDevice::Cdrom, BootDevice::Hd];
        let disks = [
            VmLaunchDiskSpec {
                path: PathBuf::from("/var/lib/libvirt/images/sys.qcow2"),
                format: DiskFormat::Qcow2,
                source: VmLaunchDiskSource::File,
                target: "vda".to_string(),
                bus: "virtio".to_string(),
                cache: None,
                io: None,
                io_threads: None,
            },
            VmLaunchDiskSpec {
                path: PathBuf::from("/dev/disk/by-id/qtr-test-disk"),
                format: DiskFormat::Raw,
                source: VmLaunchDiskSource::Block,
                target: "sda".to_string(),
                bus: "scsi".to_string(),
                cache: Some("none"),
                io: Some("threads"),
                io_threads: Some(VmLaunchIoThreadsSpec {
                    count: 4,
                    queues: 4,
                }),
            },
        ];
        let xml = build_vm_launch_domain_xml(VmLaunchDomainSpec {
            name: "install-os",
            memory_mib: 4096,
            vcpus: 2,
            io_threads: Some(VmLaunchIoThreadsSpec {
                count: 4,
                queues: 4,
            }),
            disks: &disks,
            cdrom: Some(Path::new("/isos/os.iso")),
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
        assert_eq!(
            manifest.disks[0].path,
            PathBuf::from("/var/lib/libvirt/images/sys.qcow2")
        );
        assert_eq!(manifest.disks[0].disk_type, VmDiskType::File);
        assert_eq!(manifest.disks[0].format, DiskFormat::Qcow2);
        assert_eq!(manifest.disks[0].target.as_deref(), Some("vda"));
        assert_eq!(manifest.disks[0].bus, VmDiskBus::VirtioBlk);
        assert_eq!(manifest.disks[1].disk_type, VmDiskType::Block);
        assert_eq!(
            manifest.disks[1].path,
            PathBuf::from("/dev/disk/by-id/qtr-test-disk")
        );
        assert_eq!(manifest.disks[1].format, DiskFormat::Raw);
        assert_eq!(manifest.disks[1].target.as_deref(), Some("sda"));
        assert_eq!(manifest.disks[1].bus, VmDiskBus::VirtioScsi);
        assert_eq!(manifest.disks[1].cache, Some(VmDiskCache::None));
        assert_eq!(
            manifest.disks[1].io,
            Some(VmDiskIoConfig {
                mode: VmDiskIoMode::Threads,
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
        assert_eq!(manifest.cdrom, Some(PathBuf::from("/isos/os.iso")));
        assert_eq!(
            manifest.boot,
            Some(vec!["cdrom".to_string(), "hd".to_string()])
        );
        assert_eq!(manifest.memory_gib, 4);
        assert_eq!(manifest.vcpus, 2);
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
            io_threads: None,
            disks: vec![VmDisk {
                disk_type: VmDiskType::File,
                path: PathBuf::from("/fixtures/qtr/disks/sys.qcow2"),
                format: DiskFormat::Qcow2,
                target: Some("vda".to_string()),
                bus: VmDiskBus::VirtioBlk,
                cache: Some(VmDiskCache::None),
                io: Some(VmDiskIoConfig {
                    mode: VmDiskIoMode::Native,
                }),
            }],
            cdrom: Some(PathBuf::from(
                "/fixtures/qtr/iso/CentOS-7-x86_64-DVD-2207-02.iso",
            )),
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

        let patched =
            patch_domain_xml(xml, &manifest, &boot_devices, 4096).expect("XML should patch");

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
    fn preserves_existing_disk_target_when_manifest_order_changes() {
        let manifest = test_manifest(vec![
            test_file_disk("/vm/data.qcow2", None, VmDiskBus::VirtioBlk),
            test_file_disk("/vm/sys.qcow2", None, VmDiskBus::VirtioBlk),
        ]);

        let patched = patch_domain_xml(test_domain_xml(), &manifest, &[BootDevice::Hd], 4096)
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

        let patched = patch_domain_xml(test_domain_xml(), &manifest, &[BootDevice::Hd], 4096)
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

        let xml = build_manifest_domain_xml(&manifest, &[BootDevice::Cdrom, BootDevice::Hd], 4096)
            .expect("domain XML should build");

        assert!(xml.contains("<target dev='sda' bus='sata'/>"));
        assert!(xml.contains("<target dev='sdb' bus='scsi'/>"));
        assert!(!xml.contains("<target dev='sda' bus='scsi'/>"));
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

        let patched = patch_domain_xml(&xml_with_cdrom, &manifest, &[BootDevice::Hd], 4096)
            .expect("XML should patch");

        assert!(
            patched
                .contains("<source file='/vm/scsi.qcow2'/>\n      <target dev='sdb' bus='scsi'/>")
        );
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
        manifest.disks[0].io = Some(VmDiskIoConfig {
            mode: VmDiskIoMode::Threads,
        });

        let xml = build_manifest_domain_xml(&manifest, &[BootDevice::Hd], 4096)
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
        manifest.disks[1].io = Some(VmDiskIoConfig {
            mode: VmDiskIoMode::Threads,
        });

        let patched = patch_domain_xml(test_domain_xml(), &manifest, &[BootDevice::Hd], 4096)
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
    fn rejects_removing_existing_disks_from_domain_xml() {
        let manifest = test_manifest(Vec::new());

        let error = patch_domain_xml(test_domain_xml(), &manifest, &[BootDevice::Hd], 4096)
            .expect_err("removing disks should be rejected");

        assert!(
            error
                .to_string()
                .contains("cannot remove existing domain XML disks")
        );
    }

    #[test]
    fn leaves_serial_log_unconfigured_when_manifest_omits_it() {
        let mut manifest = VmManifest {
            name: "install-os".to_string(),
            io_threads: None,
            disks: vec![VmDisk {
                disk_type: VmDiskType::File,
                path: PathBuf::from("sys.qcow2"),
                format: DiskFormat::Qcow2,
                target: None,
                bus: VmDiskBus::VirtioBlk,
                cache: None,
                io: None,
            }],
            cdrom: None,
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
        let xml = build_manifest_domain_xml(&manifest, &[BootDevice::Hd], 4096)
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

        assert_eq!(manifest.disks[0].bus, VmDiskBus::VirtioBlk);
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
            io_threads: None,
            disks,
            cdrom: None,
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
            disk_type: VmDiskType::File,
            path: PathBuf::from(path),
            format: DiskFormat::Qcow2,
            target: target.map(str::to_string),
            bus,
            cache: None,
            io: None,
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
