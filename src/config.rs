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
    #[command(about = "Run disk matrix cases serially in temporary QEMU guests")]
    Run(RunArgs),
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
