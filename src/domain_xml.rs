use std::path::Path;

use anyhow::{Result, bail};

use crate::config::GraphicsMode;
use crate::matrix::TestCase;

pub struct DomainSpec<'a> {
    pub name: &'a str,
    pub memory_mib: u64,
    pub vcpus: u32,
    pub system_disk: &'a Path,
    pub data_disk: &'a Path,
    pub network: &'a str,
    pub case: &'a TestCase,
}

pub fn build_domain_xml(spec: DomainSpec<'_>) -> String {
    format!(
        r#"<domain type='kvm'>
  <name>{name}</name>
  <memory unit='MiB'>{memory_mib}</memory>
  <currentMemory unit='MiB'>{memory_mib}</currentMemory>
  <vcpu placement='static'>{vcpus}</vcpu>
  <os>
    <type arch='x86_64'>hvm</type>
    <boot dev='hd'/>
  </os>
  <features>
    <acpi/>
    <apic/>
  </features>
  <cpu mode='host-passthrough' check='none' migratable='off'/>
  <devices>
    <disk type='file' device='disk'>
      <driver name='qemu' type='qcow2'/>
      <source file='{system_disk}'/>
      <target dev='vda' bus='virtio'/>
    </disk>
    <disk type='file' device='disk'>
      <driver name='qemu' type='raw' cache='{data_disk_cache}' io='{data_disk_io}'/>
      <source file='{data_disk}'/>
      <target dev='vdb' bus='virtio'/>
    </disk>
    <interface type='network'>
      <source network='{network}'/>
      <model type='virtio'/>
    </interface>
    <channel type='unix'>
      <target type='virtio' name='org.qemu.guest_agent.0'/>
    </channel>
    <console type='pty'>
      <target type='serial' port='0'/>
    </console>
  </devices>
</domain>
"#,
        name = escape_xml(spec.name),
        memory_mib = spec.memory_mib,
        vcpus = spec.vcpus,
        system_disk = escape_xml(&spec.system_disk.display().to_string()),
        data_disk = escape_xml(&spec.data_disk.display().to_string()),
        network = escape_xml(spec.network),
        data_disk_cache = spec.case.data_disk_cache.as_xml(),
        data_disk_io = spec.case.data_disk_io.as_xml(),
    )
}

pub struct VmLaunchDomainSpec<'a> {
    pub name: &'a str,
    pub memory_mib: u64,
    pub vcpus: u32,
    pub system_disk: &'a Path,
    pub cdrom: Option<&'a Path>,
    pub serial_log: Option<&'a Path>,
    pub boot_devices: &'a [BootDevice],
    pub network: &'a str,
    pub graphics: GraphicsSpec<'a>,
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
    let cdrom_xml = spec.cdrom.map(build_cdrom_xml).unwrap_or_default();
    let console_xml = build_console_xml(spec.serial_log);
    let graphics_xml = build_graphics_xml(spec.graphics);

    format!(
        r#"<domain type='kvm'>
  <name>{name}</name>
  <memory unit='MiB'>{memory_mib}</memory>
  <currentMemory unit='MiB'>{memory_mib}</currentMemory>
  <vcpu placement='static'>{vcpus}</vcpu>
  <os>
    <type arch='x86_64'>hvm</type>
{boot_xml}  </os>
  <features>
    <acpi/>
    <apic/>
  </features>
  <cpu mode='host-passthrough' check='none' migratable='off'/>
  <devices>
    <disk type='file' device='disk'>
      <driver name='qemu' type='qcow2'/>
      <source file='{system_disk}'/>
      <target dev='vda' bus='virtio'/>
    </disk>
{cdrom_xml}    <interface type='network'>
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
        boot_xml = boot_xml,
        system_disk = escape_xml(&spec.system_disk.display().to_string()),
        network = escape_xml(spec.network),
        cdrom_xml = cdrom_xml,
        console_xml = console_xml,
        graphics_xml = graphics_xml,
    )
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
