mod config;
mod disk;
mod domain_xml;
mod guest_agent;
mod host;
mod matrix;
mod network;
mod runner;
mod vm;

use anyhow::Result;
use clap::Parser;
use config::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Disk(args) => disk::run(args),
        Command::Host(args) => host::run(args),
        Command::Net(args) => network::run(args),
        Command::Run(args) => runner::run(args),
        Command::Vm(args) => vm::run(args),
    }
}
