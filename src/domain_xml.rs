use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::config::{DiskFormat, GraphicsMode};

pub struct VmLaunchDomainSpec<'a> {
    pub name: &'a str,
    pub memory_mib: u64,
    pub vcpus: u32,
    pub io_threads: Option<VmLaunchIoThreadsSpec>,
    pub disks: &'a [VmLaunchDiskSpec<'a>],
    pub cdrom: Option<&'a Path>,
    pub serial_log: Option<&'a Path>,
    pub boot_devices: &'a [BootDevice],
    pub network: &'a str,
    pub graphics: GraphicsSpec<'a>,
}

pub struct VmLaunchDiskSpec<'a> {
    pub path: PathBuf,
    pub format: DiskFormat,
    pub source: VmLaunchDiskSource,
    pub target: String,
    pub bus: String,
    pub cache: Option<&'a str>,
    pub io: Option<&'a str>,
    pub io_threads: Option<VmLaunchIoThreadsSpec>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmLaunchIoThreadsSpec {
    pub count: u16,
    pub queues: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmLaunchDiskSource {
    File,
    Block,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootDevice {
    Hd,
    Cdrom,
}

impl BootDevice {
    fn as_xml(self) -> &'static str {
        match self {
            Self::Hd => "hd",
            Self::Cdrom => "cdrom",
        }
    }
}

pub struct GraphicsSpec<'a> {
    pub mode: GraphicsMode,
    pub vnc_listen: &'a str,
    pub vnc_port: Option<u16>,
}

pub fn parse_boot_devices(value: &str) -> Result<Vec<BootDevice>> {
    let mut devices = Vec::new();

    for item in value.split(',') {
        let item = item.trim();
        match item {
            "hd" => devices.push(BootDevice::Hd),
            "cdrom" => devices.push(BootDevice::Cdrom),
            "" => bail!("boot device must not be empty"),
            _ => bail!("unsupported boot device {item:?}; expected hd or cdrom"),
        }
    }

    if devices.is_empty() {
        bail!("boot order must contain at least one device");
    }

    Ok(devices)
}

pub fn build_vm_launch_domain_xml(spec: VmLaunchDomainSpec<'_>) -> String {
    let boot_xml = spec
        .boot_devices
        .iter()
        .map(|device| format!("    <boot dev='{}'/>\n", device.as_xml()))
        .collect::<String>();
    let disks_xml = spec.disks.iter().map(build_disk_xml).collect::<String>();
    let io_threads_xml = spec
        .io_threads
        .map(build_domain_iothreads_xml)
        .unwrap_or_default();
    let scsi_controller_xml = build_scsi_controller_xml(spec.disks);
    let cdrom_xml = spec.cdrom.map(build_cdrom_xml).unwrap_or_default();
    let console_xml = build_console_xml(spec.serial_log);
    let graphics_xml = build_graphics_xml(spec.graphics);

    format!(
        r#"<domain type='kvm'>
  <name>{name}</name>
  <memory unit='MiB'>{memory_mib}</memory>
  <currentMemory unit='MiB'>{memory_mib}</currentMemory>
  <vcpu placement='static'>{vcpus}</vcpu>
{io_threads_xml}  <os>
    <type arch='x86_64'>hvm</type>
{boot_xml}  </os>
  <features>
    <acpi/>
    <apic/>
  </features>
  <cpu mode='host-passthrough' check='none' migratable='off'/>
  <devices>
{disks_xml}{scsi_controller_xml}{cdrom_xml}    <interface type='network'>
      <source network='{network}'/>
      <model type='virtio'/>
    </interface>
    <channel type='unix'>
      <target type='virtio' name='org.qemu.guest_agent.0'/>
    </channel>
{console_xml}{graphics_xml}  </devices>
</domain>
"#,
        name = escape_xml(spec.name),
        memory_mib = spec.memory_mib,
        vcpus = spec.vcpus,
        io_threads_xml = io_threads_xml,
        boot_xml = boot_xml,
        disks_xml = disks_xml,
        scsi_controller_xml = scsi_controller_xml,
        network = escape_xml(spec.network),
        cdrom_xml = cdrom_xml,
        console_xml = console_xml,
        graphics_xml = graphics_xml,
    )
}

fn build_domain_iothreads_xml(spec: VmLaunchIoThreadsSpec) -> String {
    format!("  <iothreads>{}</iothreads>\n", spec.count)
}

pub fn build_virtio_scsi_controller_xml(spec: Option<VmLaunchIoThreadsSpec>) -> String {
    match spec {
        Some(spec) => format!(
            r#"    <controller type='scsi' index='0' model='virtio-scsi'>
      <driver queues='{queues}'>
{mapping}      </driver>
    </controller>
"#,
            queues = spec.queues,
            mapping = build_iothreads_mapping_xml(spec, "        "),
        ),
        None => "    <controller type='scsi' index='0' model='virtio-scsi'/>\n".to_string(),
    }
}

fn build_scsi_controller_xml(disks: &[VmLaunchDiskSpec<'_>]) -> String {
    let Some(_) = disks.iter().find(|disk| disk.bus == "scsi") else {
        return String::new();
    };
    let io_threads = disks
        .iter()
        .find(|disk| disk.bus == "scsi" && disk.io == Some("threads"))
        .and_then(|disk| disk.io_threads);

    build_virtio_scsi_controller_xml(io_threads)
}

pub fn build_disk_xml(disk: &VmLaunchDiskSpec<'_>) -> String {
    let (disk_type, source_attr) = match disk.source {
        VmLaunchDiskSource::File => ("file", "file"),
        VmLaunchDiskSource::Block => ("block", "dev"),
    };
    let cache = disk
        .cache
        .map(|cache| format!(" cache='{cache}'"))
        .unwrap_or_default();
    let io = disk.io.map(|io| format!(" io='{io}'")).unwrap_or_default();
    let queues = disk
        .io_threads
        .map(|io_threads| format!(" queues='{}'", io_threads.queues))
        .unwrap_or_default();
    let driver = match disk.io_threads {
        Some(io_threads) => format!(
            r#"      <driver name='qemu' type='{format}'{cache}{io}{queues}>
{mapping}      </driver>
"#,
            format = disk.format.as_qemu_arg(),
            cache = cache,
            io = io,
            queues = queues,
            mapping = build_iothreads_mapping_xml(io_threads, "        "),
        ),
        None => format!(
            "      <driver name='qemu' type='{format}'{cache}{io}{queues}/>\n",
            format = disk.format.as_qemu_arg(),
            cache = cache,
            io = io,
            queues = queues,
        ),
    };

    format!(
        r#"    <disk type='{disk_type}' device='disk'>
{driver}      <source {source_attr}='{path}'/>
      <target dev='{target}' bus='{bus}'/>
    </disk>
"#,
        disk_type = disk_type,
        driver = driver,
        source_attr = source_attr,
        path = escape_xml(&disk.path.display().to_string()),
        target = escape_xml(&disk.target),
        bus = escape_xml(&disk.bus),
    )
}

fn build_iothreads_mapping_xml(spec: VmLaunchIoThreadsSpec, indent: &str) -> String {
    let mut xml = format!("{indent}<iothreads>\n");
    for id in 1..=spec.count {
        xml.push_str(&format!("{indent}  <iothread id='{id}'>\n"));
        for queue in (id - 1..spec.queues).step_by(usize::from(spec.count)) {
            xml.push_str(&format!("{indent}    <queue id='{queue}'/>\n"));
        }
        xml.push_str(&format!("{indent}  </iothread>\n"));
    }
    xml.push_str(&format!("{indent}</iothreads>\n"));
    xml
}

pub fn virtio_blk_disk_target(index: usize) -> String {
    format!("vd{}", disk_suffix(index))
}

pub fn virtio_scsi_disk_target(index: usize) -> String {
    format!("sd{}", disk_suffix(index))
}

fn disk_suffix(mut index: usize) -> String {
    let mut suffix = Vec::new();
    loop {
        suffix.push(char::from(
            b'a' + u8::try_from(index % 26).expect("modulo fits u8"),
        ));
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }

    suffix.reverse();
    suffix.into_iter().collect::<String>()
}

fn build_cdrom_xml(path: &Path) -> String {
    format!(
        r#"    <disk type='file' device='cdrom'>
      <driver name='qemu' type='raw'/>
      <source file='{path}'/>
      <target dev='sda' bus='sata'/>
      <readonly/>
    </disk>
"#,
        path = escape_xml(&path.display().to_string()),
    )
}

fn build_console_xml(serial_log: Option<&Path>) -> String {
    match serial_log {
        Some(path) => format!(
            r#"    <console type='file'>
      <source path='{path}'/>
      <target type='serial' port='0'/>
    </console>
"#,
            path = escape_xml(&path.display().to_string()),
        ),
        None => r#"    <console type='pty'>
      <target type='serial' port='0'/>
    </console>
"#
        .to_string(),
    }
}

fn build_graphics_xml(spec: GraphicsSpec<'_>) -> String {
    match spec.mode {
        GraphicsMode::None => String::new(),
        GraphicsMode::Vnc => {
            let port = spec
                .vnc_port
                .map(|port| port.to_string())
                .unwrap_or_else(|| "-1".to_string());
            let autoport = if spec.vnc_port.is_some() { "no" } else { "yes" };

            format!(
                r#"    <graphics type='vnc' port='{port}' autoport='{autoport}'>
      <listen type='address' address='{listen}'/>
    </graphics>
    <controller type='usb' model='qemu-xhci'/>
    <input type='tablet' bus='usb'/>
"#,
                port = port,
                autoport = autoport,
                listen = escape_xml(spec.vnc_listen),
            )
        }
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}
