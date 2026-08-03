use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::{
    config::{DiskFormat, GraphicsMode},
    domain_xml::{
        VmLaunchCpuTopology, VmLaunchDiskIoTuneSpec, VmLaunchIoThreadsSpec, VmLaunchMemorySpec,
    },
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
    pub disks: Vec<VmDiskEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cdrom: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cdroms: Option<Vec<VmCdromEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot: Option<Vec<String>>,
    #[serde(default = "default_vm_memory_gib", rename = "memoryGiB")]
    pub memory_gib: u64,
    #[serde(default = "default_vm_vcpus")]
    pub vcpus: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interfaces: Option<Vec<VmInterfaceEntry>>,
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
pub struct VmInterface {
    pub id: String,
    #[serde(rename = "type")]
    pub interface_type: VmInterfaceType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default = "default_vm_interface_model")]
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    #[serde(default, skip_serializing_if = "VmOptionalValue::is_preserve")]
    pub mode: VmOptionalValue<VmInterfaceDirectMode>,
    #[serde(default, skip_serializing_if = "VmOptionalValue::is_preserve")]
    pub vlan: VmOptionalValue<u16>,
    #[serde(default, skip_serializing_if = "VmOptionalValue::is_preserve")]
    pub mtu: VmOptionalValue<u32>,
    #[serde(default, skip_serializing_if = "VmOptionalValue::is_preserve")]
    pub link: VmOptionalValue<VmInterfaceLinkState>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VmInterfaceType {
    Network,
    Bridge,
    Direct,
    User,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VmInterfaceDirectMode {
    Vepa,
    Bridge,
    Private,
    Passthrough,
}

impl VmInterfaceDirectMode {
    pub(crate) fn as_xml(self) -> &'static str {
        match self {
            Self::Vepa => "vepa",
            Self::Bridge => "bridge",
            Self::Private => "private",
            Self::Passthrough => "passthrough",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VmInterfaceLinkState {
    Up,
    Down,
}

impl VmInterfaceLinkState {
    pub(crate) fn as_xml(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

impl VmInterfaceType {
    pub(crate) fn as_xml(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Bridge => "bridge",
            Self::Direct => "direct",
            Self::User => "user",
        }
    }

    pub(crate) fn source_attribute(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Bridge => "bridge",
            Self::Direct => "dev",
            Self::User => unreachable!("user-mode interfaces use QEMU command-line passthrough"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, untagged)]
pub enum VmInterfaceEntry {
    Present(VmInterface),
    Absent {
        id: String,
        state: VmAbsentInterfaceState,
    },
}

impl VmInterfaceEntry {
    pub fn present(interface: VmInterface) -> Self {
        Self::Present(interface)
    }

    pub fn absent(id: impl Into<String>) -> Self {
        Self::Absent {
            id: id.into(),
            state: VmAbsentInterfaceState::Absent,
        }
    }

    pub fn as_present(&self) -> Option<&VmInterface> {
        match self {
            Self::Present(interface) => Some(interface),
            Self::Absent { .. } => None,
        }
    }

    pub fn as_present_mut(&mut self) -> Option<&mut VmInterface> {
        match self {
            Self::Present(interface) => Some(interface),
            Self::Absent { .. } => None,
        }
    }

    pub fn absent_id(&self) -> Option<&str> {
        match self {
            Self::Present(_) => None,
            Self::Absent { id, .. } => Some(id),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VmAbsentInterfaceState {
    Absent,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discard: Option<VmDiskDiscard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detect_zeroes: Option<VmDiskDetectZeroes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readonly: Option<bool>,
    #[serde(default, skip_serializing_if = "VmDiskSerial::is_preserve")]
    pub serial: VmDiskSerial,
    #[serde(default, skip_serializing_if = "VmDiskIoTuneConfig::is_preserve")]
    pub io_tune: VmDiskIoTuneConfig,
}

#[derive(Clone, Debug)]
pub enum VmDiskEntry {
    Present(VmDisk),
    Absent { id: String },
}

impl VmDiskEntry {
    pub fn present(disk: VmDisk) -> Self {
        Self::Present(disk)
    }

    pub fn absent(id: impl Into<String>) -> Self {
        Self::Absent { id: id.into() }
    }

    pub fn as_present(&self) -> Option<&VmDisk> {
        match self {
            Self::Present(disk) => Some(disk),
            Self::Absent { .. } => None,
        }
    }

    pub fn as_present_mut(&mut self) -> Option<&mut VmDisk> {
        match self {
            Self::Present(disk) => Some(disk),
            Self::Absent { .. } => None,
        }
    }

    pub fn absent_id(&self) -> Option<&str> {
        match self {
            Self::Present(_) => None,
            Self::Absent { id } => Some(id),
        }
    }

    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Present(disk) => disk.id.as_deref(),
            Self::Absent { id } => Some(id),
        }
    }
}

impl Serialize for VmDiskEntry {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Present(disk) => disk.serialize(serializer),
            Self::Absent { id } => VmAbsentDeviceWire {
                id,
                state: VmDeviceState::Absent,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for VmDiskEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = VmDiskWire::deserialize(deserializer)?;
        match wire.state {
            VmDeviceState::Present => {
                let path = wire.path.ok_or_else(|| D::Error::missing_field("path"))?;
                let format = wire
                    .format
                    .ok_or_else(|| D::Error::missing_field("format"))?;
                Ok(Self::Present(VmDisk {
                    id: wire.id,
                    disk_type: wire.disk_type.unwrap_or(VmDiskType::File),
                    path,
                    format,
                    target: wire.target,
                    bus: wire.bus.unwrap_or_default(),
                    cache: wire.cache,
                    io: wire.io,
                    discard: wire.discard,
                    detect_zeroes: wire.detect_zeroes,
                    readonly: wire.readonly,
                    serial: wire.serial,
                    io_tune: wire.io_tune,
                }))
            }
            VmDeviceState::Absent => {
                let id = wire.id.ok_or_else(|| D::Error::missing_field("id"))?;
                if wire.path.is_some()
                    || wire.format.is_some()
                    || wire.disk_type.is_some()
                    || wire.target.is_some()
                    || wire.bus.is_some()
                    || wire.cache.is_some()
                    || wire.io.is_some()
                    || wire.discard.is_some()
                    || wire.detect_zeroes.is_some()
                    || wire.readonly.is_some()
                    || !wire.serial.is_preserve()
                    || !wire.io_tune.is_preserve()
                {
                    return Err(D::Error::custom("absent disk only accepts id and state"));
                }
                Ok(Self::Absent { id })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum VmDeviceState {
    #[default]
    Present,
    Absent,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VmAbsentDeviceWire<'a> {
    id: &'a str,
    state: VmDeviceState,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VmDiskWire {
    #[serde(default)]
    state: VmDeviceState,
    id: Option<String>,
    #[serde(rename = "type")]
    disk_type: Option<VmDiskType>,
    path: Option<PathBuf>,
    format: Option<DiskFormat>,
    target: Option<String>,
    bus: Option<VmDiskBus>,
    cache: Option<VmDiskCache>,
    io: Option<VmDiskIoConfig>,
    discard: Option<VmDiskDiscard>,
    detect_zeroes: Option<VmDiskDetectZeroes>,
    readonly: Option<bool>,
    #[serde(default)]
    serial: VmDiskSerial,
    #[serde(default)]
    io_tune: VmDiskIoTuneConfig,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmCdrom {
    pub id: String,
    pub media: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Clone, Debug)]
pub enum VmCdromEntry {
    Present(VmCdrom),
    Absent { id: String },
}

impl VmCdromEntry {
    pub fn present(cdrom: VmCdrom) -> Self {
        Self::Present(cdrom)
    }

    pub fn absent(id: impl Into<String>) -> Self {
        Self::Absent { id: id.into() }
    }

    pub fn as_present(&self) -> Option<&VmCdrom> {
        match self {
            Self::Present(cdrom) => Some(cdrom),
            Self::Absent { .. } => None,
        }
    }

    pub fn as_present_mut(&mut self) -> Option<&mut VmCdrom> {
        match self {
            Self::Present(cdrom) => Some(cdrom),
            Self::Absent { .. } => None,
        }
    }

    pub fn absent_id(&self) -> Option<&str> {
        match self {
            Self::Present(_) => None,
            Self::Absent { id } => Some(id),
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Present(cdrom) => &cdrom.id,
            Self::Absent { id } => id,
        }
    }
}

impl Serialize for VmCdromEntry {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Present(cdrom) => cdrom.serialize(serializer),
            Self::Absent { id } => VmAbsentDeviceWire {
                id,
                state: VmDeviceState::Absent,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for VmCdromEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = VmCdromWire::deserialize(deserializer)?;
        match wire.state {
            VmDeviceState::Present => {
                let id = wire.id.ok_or_else(|| D::Error::missing_field("id"))?;
                Ok(Self::Present(VmCdrom {
                    id,
                    media: wire.media,
                    target: wire.target,
                }))
            }
            VmDeviceState::Absent => {
                let id = wire.id.ok_or_else(|| D::Error::missing_field("id"))?;
                if wire.media.is_some() || wire.target.is_some() {
                    return Err(D::Error::custom("absent CD-ROM only accepts id and state"));
                }
                Ok(Self::Absent { id })
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VmCdromWire {
    #[serde(default)]
    state: VmDeviceState,
    id: Option<String>,
    media: Option<PathBuf>,
    target: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, utoipa::ToSchema)]
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum VmDiskType {
    File,
    Block,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, utoipa::ToSchema)]
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
#[serde(rename_all = "lowercase")]
pub enum VmDiskDiscard {
    Ignore,
    Unmap,
}

impl VmDiskDiscard {
    pub(crate) fn as_xml(self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::Unmap => "unmap",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VmDiskDetectZeroes {
    Off,
    On,
    Unmap,
}

impl VmDiskDetectZeroes {
    pub(crate) fn as_xml(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Unmap => "unmap",
        }
    }
}

pub type VmDiskSerial = VmOptionalValue<String>;
pub type VmDiskIoTuneConfig = VmOptionalValue<VmDiskIoTune>;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum VmOptionalValue<T> {
    #[default]
    Preserve,
    Remove,
    Value(T),
}

impl<T> VmOptionalValue<T> {
    pub fn configured(value: T) -> Self {
        Self::Value(value)
    }

    pub fn as_ref(&self) -> Option<&T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Preserve | Self::Remove => None,
        }
    }

    pub fn is_preserve(&self) -> bool {
        matches!(self, Self::Preserve)
    }
}

impl VmOptionalValue<String> {
    pub fn value(value: impl Into<String>) -> Self {
        Self::Value(value.into())
    }

    pub fn as_deref(&self) -> Option<&str> {
        match self {
            Self::Value(value) => Some(value),
            Self::Preserve | Self::Remove => None,
        }
    }
}

impl<T: Serialize> Serialize for VmOptionalValue<T> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Preserve | Self::Remove => serializer.serialize_none(),
            Self::Value(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for VmOptionalValue<T> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Value(value),
            None => Self::Remove,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmDiskIoTune {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes_per_sec: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_bytes_per_sec: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_bytes_per_sec: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_iops: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_iops: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_iops: Option<u64>,
}

impl VmDiskIoTune {
    pub(crate) fn launch(self) -> VmLaunchDiskIoTuneSpec {
        VmLaunchDiskIoTuneSpec {
            total_bytes_per_sec: self.total_bytes_per_sec,
            read_bytes_per_sec: self.read_bytes_per_sec,
            write_bytes_per_sec: self.write_bytes_per_sec,
            total_iops: self.total_iops,
            read_iops: self.read_iops,
            write_iops: self.write_iops,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmDiskIoConfig {
    pub mode: VmDiskIoMode,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, utoipa::ToSchema)]
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, utoipa::ToSchema)]
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

fn default_vm_interface_model() -> String {
    "virtio".to_string()
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
