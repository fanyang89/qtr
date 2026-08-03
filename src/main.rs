use anyhow::Result;
use clap::Parser;
use qtr::{
    config::{Cli, Command},
    direct_vm, disk, host, network, storage, vm, web,
};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Disk(args) => disk::run(args),
        Command::Host(args) => host::run(args),
        Command::Storage(args) => storage::run(args),
        Command::Net(args) => network::run(args),
        Command::Vm(args) => vm::run(args),
        Command::DirectVm(args) => direct_vm::run(args),
        Command::Web(args) => web::run(args),
    }
}
