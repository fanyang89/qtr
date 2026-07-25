use std::{net::SocketAddr, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(author, version, about = "QEMU/libvirt VM manager")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Create virtual disk images")]
    Disk(DiskArgs),

    #[command(about = "Configure host prerequisites")]
    Host(HostArgs),

    #[command(about = "Manage external storage backends")]
    Storage(StorageArgs),

    #[command(about = "Manage libvirt virtual networks")]
    Net(NetArgs),

    #[command(about = "Manage regular QEMU virtual machines")]
    Vm(VmArgs),

    #[command(about = "Serve the qtr Web UI and API")]
    Web(WebArgs),
}

#[derive(Debug, Args)]
pub struct WebArgs {
    /// HTTP listen address.
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub listen: SocketAddr,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,

    /// Directory containing the built Web UI assets.
    #[arg(long, default_value = "web/dist")]
    pub web_dir: PathBuf,
}

#[derive(Debug, Args)]
pub struct DiskArgs {
    #[command(subcommand)]
    pub command: DiskCommand,
}

#[derive(Debug, Subcommand)]
pub enum DiskCommand {
    #[command(about = "Print disk image information")]
    Info(DiskInfoArgs),

    #[command(about = "Create a new raw or qcow2 disk")]
    Create(DiskCreateArgs),

    #[command(about = "Create a qcow2 overlay from a backing file")]
    Overlay(DiskOverlayArgs),
}

#[derive(Debug, Args)]
pub struct DiskInfoArgs {
    /// Disk image path.
    #[arg(long)]
    pub path: PathBuf,
}

#[derive(Debug, Args)]
pub struct DiskCreateArgs {
    /// Output disk path.
    #[arg(long)]
    pub path: PathBuf,

    /// New disk format.
    #[arg(long, value_enum)]
    pub format: DiskFormat,

    /// New disk size, for example 100G.
    #[arg(long)]
    pub size: String,
}

#[derive(Debug, Args)]
pub struct DiskOverlayArgs {
    /// Output qcow2 overlay path.
    #[arg(long)]
    pub path: PathBuf,

    /// Backing image read by the overlay.
    #[arg(long)]
    pub backing_file: PathBuf,

    /// Backing image format.
    #[arg(long, value_enum, default_value_t = DiskFormat::Qcow2)]
    pub backing_format: DiskFormat,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DiskFormat {
    Raw,
    Qcow2,
}

#[derive(Debug, Args)]
pub struct HostArgs {
    #[command(subcommand)]
    pub command: HostCommand,
}

#[derive(Debug, Subcommand)]
pub enum HostCommand {
    #[command(about = "Allow a user to manage qemu:///system without a polkit agent")]
    SetupLibvirtAccess(SetupLibvirtAccessArgs),

    #[command(about = "Grant QEMU filesystem access for a VM YAML definition")]
    FixVmPerms(FixVmPermsArgs),
}

#[derive(Debug, Args)]
pub struct FixVmPermsArgs {
    /// YAML VM definition file.
    #[arg(short, long, value_name = "FILE")]
    pub file: PathBuf,

    /// QEMU process user to grant filesystem ACLs to.
    #[arg(long, default_value = "qemu")]
    pub qemu_user: String,

    /// Print actions without changing the host.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct SetupLibvirtAccessArgs {
    /// User to grant libvirt management access to. Defaults to SUDO_USER.
    #[arg(long)]
    pub user: Option<String>,

    /// Group allowed by the generated polkit rule.
    #[arg(long, default_value = "libvirt")]
    pub group: String,

    /// Polkit rule path to write.
    #[arg(long, default_value = "/etc/polkit-1/rules.d/80-qtr-libvirt.rules")]
    pub rule_path: PathBuf,

    /// QEMU process user to grant filesystem ACLs to.
    #[arg(long, default_value = "qemu")]
    pub qemu_user: String,

    /// Directory QEMU may read and write. Can be passed more than once.
    #[arg(long)]
    pub qemu_rw_dir: Vec<PathBuf>,

    /// Directory QEMU may read. Can be passed more than once.
    #[arg(long)]
    pub qemu_ro_dir: Vec<PathBuf>,

    /// Print actions without changing the host.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct StorageArgs {
    /// Storage state file.
    #[arg(long, default_value = ".qtr/storage.yaml")]
    pub config: PathBuf,

    #[command(subcommand)]
    pub command: StorageCommand,
}

#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    #[command(about = "Print host storage prerequisites")]
    Status,

    #[command(about = "Register a storage backend")]
    Add(StorageAddArgs),

    #[command(about = "List registered storage backends")]
    List,

    #[command(about = "Scan a storage backend for volumes")]
    Scan(StorageBackendArgs),

    #[command(about = "List volumes from a storage backend")]
    Volumes(StorageBackendArgs),

    #[command(about = "Connect a storage volume to the host")]
    Connect(StorageVolumeArgs),

    #[command(about = "Disconnect a storage volume from the host")]
    Disconnect(StorageVolumeArgs),
}

#[derive(Debug, Args)]
pub struct StorageAddArgs {
    #[command(subcommand)]
    pub command: StorageAddCommand,
}

#[derive(Debug, Subcommand)]
pub enum StorageAddCommand {
    #[command(about = "Register an iSCSI storage backend")]
    Iscsi(StorageAddIscsiArgs),
}

#[derive(Debug, Args)]
pub struct StorageAddIscsiArgs {
    /// Backend name shown by qtr.
    #[arg(long)]
    pub name: String,

    /// Storage service address.
    #[arg(long)]
    pub address: String,

    /// Storage service port.
    #[arg(long, default_value_t = 3260)]
    pub port: u16,
}

#[derive(Debug, Args)]
pub struct StorageBackendArgs {
    /// Backend name.
    pub name: String,
}

#[derive(Debug, Args)]
pub struct StorageVolumeArgs {
    /// Volume reference, for example backend/volume.
    pub volume: String,
}

#[derive(Debug, Args)]
pub struct NetArgs {
    #[command(subcommand)]
    pub command: NetCommand,
}

#[derive(Debug, Subcommand)]
pub enum NetCommand {
    #[command(about = "Define a NAT network")]
    Create(NetCreateArgs),

    #[command(about = "Start a defined network")]
    Start(NetNameArgs),

    #[command(about = "Stop an active network")]
    Stop(NetNameArgs),

    #[command(about = "Remove an inactive network definition")]
    Undefine(NetNameArgs),

    #[command(about = "Print network state")]
    Info(NetNameArgs),
}

#[derive(Debug, Args)]
pub struct NetCreateArgs {
    /// Libvirt network name.
    #[arg(long)]
    pub name: String,

    /// Optional host bridge name. Omit to let libvirt allocate one.
    #[arg(long)]
    pub bridge: Option<String>,

    /// Gateway address on the virtual network.
    #[arg(long, default_value = "192.168.100.1")]
    pub address: String,

    /// IPv4 netmask for the virtual network.
    #[arg(long, default_value = "255.255.255.0")]
    pub netmask: String,

    /// First DHCP address handed to guests.
    #[arg(long, default_value = "192.168.100.2")]
    pub dhcp_start: String,

    /// Last DHCP address handed to guests.
    #[arg(long, default_value = "192.168.100.254")]
    pub dhcp_end: String,

    /// Start the network after defining it.
    #[arg(long)]
    pub start: bool,

    /// Start the network automatically when libvirt starts.
    #[arg(long)]
    pub autostart: bool,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,
}

#[derive(Debug, Args)]
pub struct NetNameArgs {
    /// Libvirt network name.
    pub name: String,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,
}

#[derive(Debug, Args)]
pub struct VmArgs {
    #[command(subcommand)]
    pub command: VmCommand,
}

#[derive(Debug, Subcommand)]
pub enum VmCommand {
    #[command(about = "Print VM capabilities reported by libvirt")]
    Capabilities(VmCapabilitiesArgs),

    #[command(about = "Write a starter VM YAML definition")]
    Init(VmInitArgs),

    #[command(about = "Apply a VM definition from a YAML file")]
    Apply(VmApplyArgs),

    #[command(about = "Dump a defined VM as a YAML file")]
    Dump(VmDumpArgs),

    #[command(about = "List defined VMs")]
    List(VmListArgs),

    #[command(about = "Start a defined VM")]
    Start(VmStartArgs),

    #[command(about = "Stop a running VM")]
    Stop(VmStopArgs),

    #[command(about = "Remove an inactive VM definition")]
    Rm(VmRemoveArgs),

    #[command(about = "Print the VNC endpoint for a running VM")]
    Vnc(VmNameArgs),

    #[command(about = "Run a shell command through QEMU Guest Agent")]
    Exec(VmExecArgs),

    #[command(about = "Copy one file between host and guest")]
    Cp(VmCpArgs),
}

#[derive(Debug, Args)]
pub struct VmCapabilitiesArgs {
    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,

    /// Guest architecture to query. Omit to use the libvirt default.
    #[arg(long)]
    pub arch: Option<String>,

    /// QEMU machine type to query. Omit to use the libvirt default.
    #[arg(long)]
    pub machine: Option<String>,

    /// Libvirt virtualization type.
    #[arg(long, default_value = "kvm")]
    pub virtualization: String,

    /// Print machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct VmInitArgs {
    /// Libvirt domain name used in the template.
    #[arg(long, default_value = "install-os")]
    pub name: String,

    /// Output YAML file. Omit to write to stdout.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Disk path used in the template. May be passed multiple times.
    #[arg(long, value_name = "PATH")]
    pub disk: Vec<PathBuf>,

    /// Installation ISO path used in the template.
    #[arg(long, default_value = "/path/to/installer.iso")]
    pub cdrom: PathBuf,

    /// Generate a hard-disk-only template without an installer ISO.
    #[arg(long)]
    pub no_cdrom: bool,

    /// Guest memory size in GiB.
    #[arg(long, default_value_t = 4)]
    pub memory_gib: u64,

    /// Number of guest vCPUs.
    #[arg(long, default_value_t = 2)]
    pub vcpus: u32,

    /// Libvirt network attached to the VM.
    #[arg(long, default_value = "default")]
    pub network: String,

    /// Address VNC listens on.
    #[arg(long, default_value = "127.0.0.1")]
    pub vnc_listen: String,
}

#[derive(Debug, Args)]
pub struct VmApplyArgs {
    /// YAML VM definition file.
    #[arg(short, long, value_name = "FILE")]
    pub file: PathBuf,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,

    /// Start the VM after applying the definition.
    #[arg(long)]
    pub start: bool,

    /// Wait until the started VM shuts down.
    #[arg(long)]
    pub wait_shutdown: bool,

    /// Undefine the VM after --wait-shutdown completes.
    #[arg(long)]
    pub rm_after_shutdown: bool,

    /// Maximum seconds to wait for guest shutdown (default: wait forever).
    #[arg(long, value_name = "SECS")]
    pub shutdown_timeout_secs: Option<u64>,

    /// Print the libvirt domain XML diff without applying it.
    #[arg(long)]
    pub dry_run: bool,

    /// When to color dry-run diffs.
    #[arg(long, value_enum, default_value_t = ColorMode::Auto)]
    pub color: ColorMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Args)]
pub struct VmDumpArgs {
    /// Libvirt domain name.
    pub name: String,

    /// Output YAML file. Omit to write to stdout.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Print raw inactive libvirt domain XML instead of YAML.
    #[arg(long)]
    pub xml: bool,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,
}

#[derive(Debug, Args)]
pub struct VmListArgs {
    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,
}

#[derive(Debug, Args)]
pub struct VmNameArgs {
    /// Libvirt domain name.
    pub name: String,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,
}

#[derive(Debug, Args)]
pub struct VmStartArgs {
    /// Libvirt domain name.
    pub name: String,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,

    /// Wait until the guest shuts down.
    #[arg(long)]
    pub wait_shutdown: bool,

    /// Undefine the VM after --wait-shutdown completes.
    #[arg(long)]
    pub rm_after_shutdown: bool,

    /// Maximum seconds to wait for guest shutdown (default: wait forever).
    #[arg(long, value_name = "SECS")]
    pub shutdown_timeout_secs: Option<u64>,
}

#[derive(Debug, Args)]
pub struct VmStopArgs {
    /// Libvirt domain name.
    pub name: String,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,

    /// Force stop the VM instead of graceful shutdown.
    #[arg(long)]
    pub force: bool,

    /// Wait until the guest becomes inactive.
    #[arg(long)]
    pub wait: bool,

    /// Maximum seconds to wait for guest shutdown (default: wait forever).
    #[arg(long, value_name = "SECS")]
    pub shutdown_timeout_secs: Option<u64>,
}

#[derive(Debug, Args)]
pub struct VmRemoveArgs {
    /// Libvirt domain name.
    pub name: String,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,

    /// Force stop the VM before removing its definition.
    #[arg(long)]
    pub force_stop: bool,
}

#[derive(Debug, Args)]
pub struct VmExecArgs {
    /// Libvirt domain name.
    pub name: String,

    /// Seconds to wait for QEMU Guest Agent and command completion.
    #[arg(long, default_value_t = 120)]
    pub timeout_secs: u64,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,

    /// Local script file uploaded and executed inside the guest.
    #[arg(long, value_name = "FILE")]
    pub script: Option<PathBuf>,

    /// Write execution result as JSON instead of streaming stdout/stderr.
    #[arg(long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Command executed inside the guest; each argument is passed verbatim (use sh -c for shell features).
    #[arg(last = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct VmCpArgs {
    /// Libvirt domain name.
    pub name: String,

    /// Source path. Prefix guest paths with guest:.
    pub source: String,

    /// Destination path. Prefix guest paths with guest:.
    pub dest: String,

    /// Seconds to wait for QEMU Guest Agent.
    #[arg(long, default_value_t = 120)]
    pub timeout_secs: u64,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,

    /// Create the destination parent directory before copying.
    #[arg(long)]
    pub parents: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsMode {
    None,
    Vnc,
}
