use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};

use crate::config::{DiskArgs, DiskCommand, DiskFormat};

pub fn run(args: DiskArgs) -> Result<()> {
    match args.command {
        DiskCommand::Create(args) => {
            create_image(&args.path, args.format, &args.size)?;
            eprintln!("[qtr] created disk: {}", args.path.display());
            Ok(())
        }
        DiskCommand::Overlay(args) => {
            create_overlay(&args.path, &args.backing_file, args.backing_format)?;
            eprintln!("[qtr] created overlay: {}", args.path.display());
            Ok(())
        }
    }
}

pub fn create_image(path: &Path, format: DiskFormat, size: &str) -> Result<()> {
    prepare_output(path)?;

    let status = Command::new("qemu-img")
        .arg("create")
        .arg("-f")
        .arg(format.as_qemu_arg())
        .arg(path)
        .arg(size)
        .status()
        .with_context(|| format!("failed to run qemu-img for {}", path.display()))?;

    if !status.success() {
        bail!("qemu-img failed to create disk {}", path.display());
    }

    Ok(())
}

pub fn create_overlay(path: &Path, backing_file: &Path, backing_format: DiskFormat) -> Result<()> {
    if !backing_file.exists() {
        bail!("backing file {} does not exist", backing_file.display());
    }

    prepare_output(path)?;

    let status = Command::new("qemu-img")
        .arg("create")
        .arg("-f")
        .arg(DiskFormat::Qcow2.as_qemu_arg())
        .arg("-F")
        .arg(backing_format.as_qemu_arg())
        .arg("-b")
        .arg(backing_file)
        .arg(path)
        .status()
        .with_context(|| format!("failed to run qemu-img for {}", path.display()))?;

    if !status.success() {
        bail!("qemu-img failed to create overlay {}", path.display());
    }

    Ok(())
}

fn prepare_output(path: &Path) -> Result<()> {
    if path.exists() {
        bail!("disk {} already exists", path.display());
    }

    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    Ok(())
}

impl DiskFormat {
    pub fn as_qemu_arg(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Qcow2 => "qcow2",
        }
    }
}
