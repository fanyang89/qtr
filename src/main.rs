mod config;
mod domain_xml;
mod guest_agent;
mod matrix;
mod runner;
mod vm;

use anyhow::Result;
use clap::Parser;
use config::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Run(args) => runner::run(args),
        Command::Vm(args) => vm::run(args),
    }
}
