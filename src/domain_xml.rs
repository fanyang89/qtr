use std::path::Path;

use crate::matrix::TestCase;

pub struct DomainSpec<'a> {
    pub name: &'a str,
    pub memory_mib: u64,
    pub vcpus: u32,
    pub system_disk: &'a Path,
    pub data_disk: &'a Path,
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
      <source network='default'/>
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
        data_disk_cache = spec.case.data_disk_cache.as_xml(),
        data_disk_io = spec.case.data_disk_io.as_xml(),
    )
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}
