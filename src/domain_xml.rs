use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::config::{DiskFormat, GraphicsMode};

pub struct VmLaunchDomainSpec<'a> {
    pub name: &'a str,
    pub machine: Option<&'a str>,
    pub memory: VmLaunchMemorySpec,
    pub vcpus: u32,
    pub cpu: Option<VmLaunchCpuSpec<'a>>,
    pub io_threads: Option<VmLaunchIoThreadsSpec>,
    pub disks: &'a [VmLaunchDiskSpec<'a>],
    pub cdroms: &'a [VmLaunchCdromSpec<'a>],
    pub serial_log: Option<&'a Path>,
    pub boot_devices: &'a [BootDevice],
    pub network: &'a str,
    pub graphics: GraphicsSpec<'a>,
}

#[derive(Clone, Copy, Debug)]
pub struct VmLaunchCdromSpec<'a> {
    pub id: &'a str,
    pub media: Option<&'a Path>,
    pub target: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmLaunchMemorySpec {
    pub size_mib: u64,
    pub max_mib: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmLaunchCpuSpec<'a> {
    pub mode: &'a str,
    pub model: Option<&'a str>,
    pub topology: Option<VmLaunchCpuTopology>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmLaunchCpuTopology {
    pub sockets: u32,
    pub cores: u32,
    pub threads: u32,
}

pub struct VmLaunchDiskSpec<'a> {
    pub id: Option<&'a str>,
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
    let cdroms_xml = spec.cdroms.iter().map(build_cdrom_xml).collect::<String>();
    let console_xml = build_console_xml(spec.serial_log);
    let graphics_xml = build_graphics_xml(spec.graphics);
    let machine = spec
        .machine
        .map(|machine| format!(" machine='{}'", escape_xml(machine)))
        .unwrap_or_default();
    let max_memory_mib = spec.memory.max_mib.unwrap_or(spec.memory.size_mib);
    let cpu_xml = spec.cpu.map(build_cpu_xml).unwrap_or_default();

    format!(
        r#"<domain type='kvm'>
  <name>{name}</name>
  <memory unit='MiB'>{max_memory_mib}</memory>
  <currentMemory unit='MiB'>{memory_mib}</currentMemory>
  <vcpu placement='static'>{vcpus}</vcpu>
{io_threads_xml}  <os>
    <type arch='x86_64'{machine}>hvm</type>
{boot_xml}  </os>
  <features>
    <acpi/>
    <apic/>
  </features>
{cpu_xml}  <devices>
{disks_xml}{scsi_controller_xml}{cdroms_xml}    <interface type='network'>
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
        max_memory_mib = max_memory_mib,
        memory_mib = spec.memory.size_mib,
        vcpus = spec.vcpus,
        machine = machine,
        cpu_xml = cpu_xml,
        io_threads_xml = io_threads_xml,
        boot_xml = boot_xml,
        disks_xml = disks_xml,
        scsi_controller_xml = scsi_controller_xml,
        network = escape_xml(spec.network),
        cdroms_xml = cdroms_xml,
        console_xml = console_xml,
        graphics_xml = graphics_xml,
    )
}

pub fn build_cpu_xml(spec: VmLaunchCpuSpec<'_>) -> String {
    let attributes = match spec.mode {
        "host-passthrough" => " mode='host-passthrough' check='none' migratable='off'".to_string(),
        "host-model" => " mode='host-model'".to_string(),
        "custom" => " mode='custom' match='exact'".to_string(),
        mode => format!(" mode='{}'", escape_xml(mode)),
    };
    let model = spec
        .model
        .map(|model| {
            format!(
                "    <model fallback='forbid'>{}</model>\n",
                escape_xml(model)
            )
        })
        .unwrap_or_default();
    let topology = spec
        .topology
        .map(|topology| {
            format!(
                "    <topology sockets='{}' cores='{}' threads='{}'/>\n",
                topology.sockets, topology.cores, topology.threads
            )
        })
        .unwrap_or_default();

    if model.is_empty() && topology.is_empty() {
        format!("  <cpu{attributes}/>\n")
    } else {
        format!("  <cpu{attributes}>\n{model}{topology}  </cpu>\n")
    }
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
    let alias = disk
        .id
        .map(|id| format!("      <alias name='ua-qtr-disk-{}'/>\n", escape_xml(id)))
        .unwrap_or_default();

    format!(
        r#"    <disk type='{disk_type}' device='disk'>
{driver}      <source {source_attr}='{path}'/>
      <target dev='{target}' bus='{bus}'/>
{alias}    </disk>
"#,
        disk_type = disk_type,
        driver = driver,
        source_attr = source_attr,
        path = escape_xml(&disk.path.display().to_string()),
        target = escape_xml(&disk.target),
        bus = escape_xml(&disk.bus),
        alias = alias,
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

pub const CDROM_TARGET: &str = "sda";

pub fn build_cdrom_xml(cdrom: &VmLaunchCdromSpec<'_>) -> String {
    let source = cdrom
        .media
        .map(|path| {
            format!(
                "      <source file='{}'/>\n",
                escape_xml(&path.display().to_string())
            )
        })
        .unwrap_or_default();
    format!(
        r#"    <disk type='file' device='cdrom'>
      <driver name='qemu' type='raw'/>
{source}      <target dev='{target}' bus='sata'/>
      <readonly/>
      <alias name='ua-qtr-cdrom-{id}'/>
    </disk>
"#,
        source = source,
        target = escape_xml(cdrom.target),
        id = escape_xml(cdrom.id),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn disk_spec(target: &str, bus: &str) -> VmLaunchDiskSpec<'static> {
        VmLaunchDiskSpec {
            id: None,
            path: PathBuf::from("/var/lib/libvirt/images/sys.qcow2"),
            format: DiskFormat::Qcow2,
            source: VmLaunchDiskSource::File,
            target: target.to_string(),
            bus: bus.to_string(),
            cache: None,
            io: None,
            io_threads: None,
        }
    }

    #[test]
    fn disk_suffix_follows_libvirt_device_naming() {
        let cases = [
            (0, "vda"),
            (1, "vdb"),
            (25, "vdz"),
            (26, "vdaa"),
            (27, "vdab"),
            (51, "vdaz"),
            (52, "vdba"),
            (702, "vdaaa"), // 26 + 26*26 wraps to a third letter
        ];
        for (index, expected) in cases {
            assert_eq!(virtio_blk_disk_target(index), expected, "index {index}");
        }
        assert_eq!(virtio_scsi_disk_target(0), "sda");
        assert_eq!(virtio_scsi_disk_target(26), "sdaa");
    }

    #[test]
    fn parses_boot_devices() {
        assert_eq!(parse_boot_devices("hd").unwrap(), vec![BootDevice::Hd]);
        assert_eq!(
            parse_boot_devices("cdrom,hd").unwrap(),
            vec![BootDevice::Cdrom, BootDevice::Hd]
        );
        assert_eq!(
            parse_boot_devices(" hd , cdrom ").unwrap(),
            vec![BootDevice::Hd, BootDevice::Cdrom]
        );
    }

    #[test]
    fn rejects_invalid_boot_devices() {
        for value in ["", "hd,", ",hd", "hd,,cdrom", "foo", "HD"] {
            assert!(parse_boot_devices(value).is_err(), "value {value:?}");
        }
    }

    #[test]
    fn escapes_all_xml_special_characters() {
        assert_eq!(
            escape_xml("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
        assert_eq!(escape_xml("plain"), "plain");
    }

    #[test]
    fn maps_queues_to_iothreads_round_robin() {
        let xml = build_iothreads_mapping_xml(
            VmLaunchIoThreadsSpec {
                count: 2,
                queues: 4,
            },
            "",
        );
        assert_eq!(
            xml,
            "<iothreads>\n  <iothread id='1'>\n    <queue id='0'/>\n    <queue id='2'/>\n  </iothread>\n  <iothread id='2'>\n    <queue id='1'/>\n    <queue id='3'/>\n  </iothread>\n</iothreads>\n"
        );
    }

    #[test]
    fn maps_fewer_queues_than_iothreads() {
        let xml = build_iothreads_mapping_xml(
            VmLaunchIoThreadsSpec {
                count: 4,
                queues: 2,
            },
            "",
        );
        assert!(xml.contains("<iothread id='1'>\n    <queue id='0'/>\n  </iothread>"));
        assert!(xml.contains("<iothread id='2'>\n    <queue id='1'/>\n  </iothread>"));
        assert!(xml.contains("<iothread id='3'>\n  </iothread>"));
        assert!(xml.contains("<iothread id='4'>\n  </iothread>"));
    }

    #[test]
    fn builds_disk_xml_for_file_and_block_sources() {
        let mut file = disk_spec("vda", "virtio");
        file.id = Some("root");
        let file_xml = build_disk_xml(&file);
        assert!(file_xml.contains("<disk type='file' device='disk'>"));
        assert!(file_xml.contains("<source file='/var/lib/libvirt/images/sys.qcow2'/>"));
        assert!(file_xml.contains("<target dev='vda' bus='virtio'/>"));
        assert!(file_xml.contains("<alias name='ua-qtr-disk-root'/>"));

        let mut block = disk_spec("sda", "scsi");
        block.source = VmLaunchDiskSource::Block;
        block.path = PathBuf::from("/dev/disk/by-id/test");
        let block_xml = build_disk_xml(&block);
        assert!(block_xml.contains("<disk type='block' device='disk'>"));
        assert!(block_xml.contains("<source dev='/dev/disk/by-id/test'/>"));
    }

    #[test]
    fn disk_xml_escapes_paths() {
        let mut disk = disk_spec("vda", "virtio");
        disk.path = PathBuf::from("/tmp/a&b.qcow2");
        let xml = build_disk_xml(&disk);
        assert!(xml.contains("<source file='/tmp/a&amp;b.qcow2'/>"));
    }

    #[test]
    fn domain_xml_only_adds_scsi_controller_for_scsi_disks() {
        let boot_devices = [BootDevice::Hd];
        let virtio_disks = [disk_spec("vda", "virtio")];
        let xml = build_vm_launch_domain_xml(VmLaunchDomainSpec {
            name: "test",
            machine: None,
            memory: VmLaunchMemorySpec {
                size_mib: 1024,
                max_mib: None,
            },
            vcpus: 1,
            cpu: None,
            io_threads: None,
            disks: &virtio_disks,
            cdroms: &[],
            serial_log: None,
            boot_devices: &boot_devices,
            network: "default",
            graphics: GraphicsSpec {
                mode: GraphicsMode::None,
                vnc_listen: "127.0.0.1",
                vnc_port: None,
            },
        });
        assert!(!xml.contains("virtio-scsi"));

        let scsi_disks = [disk_spec("sda", "scsi")];
        let xml = build_vm_launch_domain_xml(VmLaunchDomainSpec {
            name: "test",
            machine: None,
            memory: VmLaunchMemorySpec {
                size_mib: 1024,
                max_mib: None,
            },
            vcpus: 1,
            cpu: None,
            io_threads: None,
            disks: &scsi_disks,
            cdroms: &[],
            serial_log: None,
            boot_devices: &boot_devices,
            network: "default",
            graphics: GraphicsSpec {
                mode: GraphicsMode::None,
                vnc_listen: "127.0.0.1",
                vnc_port: None,
            },
        });
        assert!(xml.contains("<controller type='scsi' index='0' model='virtio-scsi'/>"));
    }

    #[test]
    fn builds_loaded_and_empty_cdrom_trays() {
        let loaded = build_cdrom_xml(&VmLaunchCdromSpec {
            id: "installer",
            media: Some(Path::new("/isos/os&tools.iso")),
            target: "sda",
        });
        let empty = build_cdrom_xml(&VmLaunchCdromSpec {
            id: "tools",
            media: None,
            target: "sdb",
        });

        assert!(loaded.contains("<source file='/isos/os&amp;tools.iso'/>"));
        assert!(loaded.contains("<target dev='sda' bus='sata'/>"));
        assert!(loaded.contains("<alias name='ua-qtr-cdrom-installer'/>"));
        assert!(!empty.contains("<source"));
        assert!(empty.contains("<target dev='sdb' bus='sata'/>"));
        assert!(empty.contains("<readonly/>"));
        assert!(empty.contains("<alias name='ua-qtr-cdrom-tools'/>"));
    }

    #[test]
    fn builds_machine_cpu_topology_and_memory_range() {
        let boot_devices = [BootDevice::Hd];
        let disks = [disk_spec("vda", "virtio")];
        let xml = build_vm_launch_domain_xml(VmLaunchDomainSpec {
            name: "test",
            machine: Some("pc-q35-10.0"),
            memory: VmLaunchMemorySpec {
                size_mib: 4096,
                max_mib: Some(8192),
            },
            vcpus: 8,
            cpu: Some(VmLaunchCpuSpec {
                mode: "custom",
                model: Some("EPYC-Milan"),
                topology: Some(VmLaunchCpuTopology {
                    sockets: 2,
                    cores: 2,
                    threads: 2,
                }),
            }),
            io_threads: None,
            disks: &disks,
            cdroms: &[],
            serial_log: None,
            boot_devices: &boot_devices,
            network: "default",
            graphics: GraphicsSpec {
                mode: GraphicsMode::None,
                vnc_listen: "127.0.0.1",
                vnc_port: None,
            },
        });

        assert!(xml.contains("<memory unit='MiB'>8192</memory>"));
        assert!(xml.contains("<currentMemory unit='MiB'>4096</currentMemory>"));
        assert!(xml.contains("<vcpu placement='static'>8</vcpu>"));
        assert!(xml.contains("<type arch='x86_64' machine='pc-q35-10.0'>hvm</type>"));
        assert!(xml.contains("<cpu mode='custom' match='exact'>"));
        assert!(xml.contains("<model fallback='forbid'>EPYC-Milan</model>"));
        assert!(xml.contains("<topology sockets='2' cores='2' threads='2'/>"));
    }

    #[test]
    fn graphics_xml_sets_autoport_only_without_explicit_port() {
        let spec = GraphicsSpec {
            mode: GraphicsMode::Vnc,
            vnc_listen: "127.0.0.1",
            vnc_port: None,
        };
        let xml = build_graphics_xml(spec);
        assert!(xml.contains("port='-1' autoport='yes'"));

        let spec = GraphicsSpec {
            mode: GraphicsMode::Vnc,
            vnc_listen: "127.0.0.1",
            vnc_port: Some(5901),
        };
        let xml = build_graphics_xml(spec);
        assert!(xml.contains("port='5901' autoport='no'"));

        let spec = GraphicsSpec {
            mode: GraphicsMode::None,
            vnc_listen: "127.0.0.1",
            vnc_port: None,
        };
        assert!(build_graphics_xml(spec).is_empty());
    }
}
