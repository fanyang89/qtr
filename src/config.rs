use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(author, version, about = "QEMU test runner")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Create virtual disk images")]
    Disk(DiskArgs),

    #[command(about = "Manage libvirt virtual networks")]
    Net(NetArgs),

    #[command(about = "Run disk matrix cases serially in temporary QEMU guests")]
    Run(RunArgs),

    #[command(about = "Manage regular QEMU virtual machines")]
    Vm(VmArgs),
}

#[derive(Debug, Args)]
pub struct DiskArgs {
    #[command(subcommand)]
    pub command: DiskCommand,
}

#[derive(Debug, Subcommand)]
pub enum DiskCommand {
    #[command(about = "Create a new raw or qcow2 disk")]
    Create(DiskCreateArgs),

    #[command(about = "Create a qcow2 overlay from a backing file")]
    Overlay(DiskOverlayArgs),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum DiskFormat {
    Raw,
    Qcow2,
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
    #[command(about = "Define a regular VM without starting it")]
    Create(VmCreateArgs),

    #[command(about = "Create and start a regular VM")]
    Launch(VmLaunchArgs),

    #[command(about = "Start a defined VM")]
    Start(VmNameArgs),

    #[command(about = "Print the VNC endpoint for a running VM")]
    Vnc(VmNameArgs),

    #[command(about = "Wait until a VM shuts down")]
    WaitShutdown(VmNameArgs),

    #[command(about = "Ask a VM to shut down gracefully")]
    Shutdown(VmShutdownArgs),

    #[command(about = "Force stop a VM")]
    Destroy(VmNameArgs),

    #[command(about = "Remove an inactive VM definition")]
    Undefine(VmNameArgs),
}

#[derive(Debug, Args)]
pub struct VmCreateArgs {
    /// Libvirt domain name.
    #[arg(long)]
    pub name: String,

    /// qcow2 system disk path.
    #[arg(long)]
    pub system_disk: PathBuf,

    /// Create the qcow2 system disk with this size, for example 40G.
    #[arg(long)]
    pub create_system_disk: Option<String>,

    /// Optional installation ISO attached as a readonly cdrom.
    #[arg(long)]
    pub cdrom: Option<PathBuf>,

    /// Boot order as comma-separated devices: hd, cdrom.
    #[arg(long)]
    pub boot: Option<String>,

    /// Guest memory size in MiB.
    #[arg(long, default_value_t = 4096)]
    pub memory_mib: u64,

    /// Number of guest vCPUs.
    #[arg(long, default_value_t = 2)]
    pub vcpus: u32,

    /// Graphics device exposed by the VM.
    #[arg(long, value_enum, default_value_t = GraphicsMode::Vnc)]
    pub graphics: GraphicsMode,

    /// Address VNC listens on when --graphics vnc is used.
    #[arg(long, default_value = "127.0.0.1")]
    pub vnc_listen: String,

    /// Fixed VNC port. Omit to let libvirt auto-assign one.
    #[arg(long)]
    pub vnc_port: Option<u16>,

    /// Libvirt network attached to the VM.
    #[arg(long, default_value = "default")]
    pub network: String,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,
}

#[derive(Debug, Args)]
pub struct VmLaunchArgs {
    #[command(flatten)]
    pub create: VmCreateArgs,

    /// Wait until the guest shuts down, then undefine the VM and keep the disk.
    #[arg(long)]
    pub wait_shutdown: bool,
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
pub struct VmShutdownArgs {
    /// Libvirt domain name.
    pub name: String,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,

    /// Wait until the guest becomes inactive.
    #[arg(long)]
    pub wait: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum GraphicsMode {
    None,
    Vnc,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// TOML file containing [[case]] entries.
    #[arg(long)]
    pub matrix: PathBuf,

    /// Base qcow2 image used to create per-case system disk overlays.
    #[arg(long)]
    pub system_base_image: PathBuf,

    /// Directory where per-case raw data disks are created.
    #[arg(long)]
    pub data_disk_dir: PathBuf,

    /// Size passed to qemu-img for each raw data disk, for example 100G.
    #[arg(long)]
    pub data_disk_size: String,

    /// Fixed command executed inside the guest via /bin/sh -lc.
    #[arg(long)]
    pub test_cmd: String,

    /// Libvirt connection URI.
    #[arg(long, default_value = "qemu:///system")]
    pub connect_uri: String,

    /// Libvirt network attached to each test VM.
    #[arg(long, default_value = "default")]
    pub network: String,

    /// Directory where per-case system overlays are created.
    #[arg(long, default_value = "/var/lib/libvirt/images/qtr-runs")]
    pub workdir: PathBuf,

    /// Guest memory size in MiB.
    #[arg(long, default_value_t = 2048)]
    pub memory_mib: u64,

    /// Number of guest vCPUs.
    #[arg(long, default_value_t = 2)]
    pub vcpus: u32,

    /// Cleanup VM definitions and disks.
    #[arg(long, value_enum, default_value_t = CleanupPolicy::OnSuccess)]
    pub cleanup: CleanupPolicy,

    /// Seconds to wait for QEMU Guest Agent to become ready.
    #[arg(long, default_value_t = 120)]
    pub agent_timeout_secs: u64,

    /// Seconds to wait for the guest test command to exit.
    #[arg(long, default_value_t = 3600)]
    pub test_timeout_secs: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CleanupPolicy {
    Always,
    Never,
    OnSuccess,
}

impl CleanupPolicy {
    pub fn should_cleanup(self, success: bool) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::OnSuccess => success,
        }
    }
}
