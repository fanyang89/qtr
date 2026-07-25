use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    config::{DiskFormat, GraphicsMode},
    domain_xml::{VmLaunchCpuTopology, VmLaunchIoThreadsSpec, VmLaunchMemorySpec},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmManifest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine: Option<VmMachine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<VmCpu>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<VmMemory>,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmMachine {
    #[serde(rename = "type")]
    pub machine_type: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmCpu {
    #[serde(default)]
    pub mode: VmCpuMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcpus: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology: Option<VmCpuTopology>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VmCpuMode {
    #[default]
    HostPassthrough,
    HostModel,
    Custom,
}

impl VmCpuMode {
    pub(crate) fn as_xml(self) -> &'static str {
        match self {
            Self::HostPassthrough => "host-passthrough",
            Self::HostModel => "host-model",
            Self::Custom => "custom",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmCpuTopology {
    pub sockets: u32,
    pub cores: u32,
    pub threads: u32,
}

impl VmCpuTopology {
    pub(crate) fn vcpus(self) -> Result<u32> {
        self.sockets
            .checked_mul(self.cores)
            .and_then(|value| value.checked_mul(self.threads))
            .context("CPU topology is too large")
    }

    pub(crate) fn launch(self) -> VmLaunchCpuTopology {
        VmLaunchCpuTopology {
            sockets: self.sockets,
            cores: self.cores,
            threads: self.threads,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmMemory {
    #[serde(rename = "sizeMiB")]
    pub size_mib: u64,
    #[serde(rename = "maxMiB", skip_serializing_if = "Option::is_none")]
    pub max_mib: Option<u64>,
}

impl VmMemory {
    pub(crate) fn launch(self) -> VmLaunchMemorySpec {
        VmLaunchMemorySpec {
            size_mib: self.size_mib,
            max_mib: self.max_mib,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmDisk {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
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
    pub(crate) fn effective(self) -> VmLaunchIoThreadsSpec {
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
    pub(crate) fn as_xml(self) -> &'static str {
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
    pub(crate) fn as_xml(self) -> &'static str {
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
    pub(crate) fn target_bus(self) -> &'static str {
        match self {
            Self::VirtioBlk => "virtio",
            Self::VirtioScsi => "scsi",
        }
    }
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
