use std::{ffi::OsString, fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::config::{DiskArgs, DiskCommand, DiskFormat};

pub fn run(args: DiskArgs) -> Result<()> {
    match args.command {
        DiskCommand::Info(args) => info(&args.path),
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

fn info(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("disk {} does not exist", path.display());
    }

    let info = qemu_img_info(path)?;

    println!("path: {}", path.display());
    println!("format: {}", info.format.as_deref().unwrap_or("-"));
    println!("virtual-size: {}", format_optional_bytes(info.virtual_size));
    println!("actual-size: {}", format_optional_bytes(info.actual_size));
    println!(
        "backing-file: {}",
        info.backing_filename.as_deref().unwrap_or("-")
    );
    println!(
        "backing-format: {}",
        info.backing_filename_format.as_deref().unwrap_or("-")
    );

    Ok(())
}

fn qemu_img_info(path: &Path) -> Result<QemuImgInfo> {
    let output = duct::cmd(
        "qemu-img",
        [
            OsString::from("info"),
            OsString::from("--output=json"),
            path.as_os_str().to_os_string(),
        ],
    )
    .read()
    .with_context(|| format!("failed to run qemu-img info for {}", path.display()))?;
    parse_qemu_img_info(&output)
        .with_context(|| format!("failed to parse qemu-img info for {}", path.display()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ImageInfo {
    pub format: DiskFormat,
    pub virtual_size_bytes: u64,
}

pub(crate) fn image_info(path: &Path) -> Result<ImageInfo> {
    let info = qemu_img_info(path)?;
    let format = match info.format.as_deref() {
        Some("raw") => DiskFormat::Raw,
        Some("qcow2") => DiskFormat::Qcow2,
        Some(format) => bail!("unsupported disk image format {format:?}"),
        None => bail!("qemu-img did not report an image format"),
    };
    let virtual_size_bytes = info.virtual_size.with_context(|| {
        format!(
            "qemu-img did not report virtual size for {}",
            path.display()
        )
    })?;
    Ok(ImageInfo {
        format,
        virtual_size_bytes,
    })
}

pub(crate) fn image_virtual_size(path: &Path) -> Result<u64> {
    Ok(image_info(path)?.virtual_size_bytes)
}

pub(crate) fn resize_image(path: &Path, format: DiskFormat, size_bytes: u64) -> Result<()> {
    duct::cmd(
        "qemu-img",
        [
            OsString::from("resize"),
            OsString::from("-f"),
            OsString::from(format.as_qemu_arg()),
            path.as_os_str().to_os_string(),
            OsString::from(size_bytes.to_string()),
        ],
    )
    .run()
    .with_context(|| format!("failed to resize disk image {}", path.display()))?;
    Ok(())
}

pub fn create_image(path: &Path, format: DiskFormat, size: &str) -> Result<()> {
    prepare_output(path)?;

    duct::cmd(
        "qemu-img",
        [
            OsString::from("create"),
            OsString::from("-f"),
            OsString::from(format.as_qemu_arg()),
            path.as_os_str().to_os_string(),
            OsString::from(size),
        ],
    )
    .run()
    .with_context(|| format!("failed to run qemu-img for {}", path.display()))?;

    Ok(())
}

pub fn create_overlay(path: &Path, backing_file: &Path, backing_format: DiskFormat) -> Result<()> {
    if !backing_file.exists() {
        bail!("backing file {} does not exist", backing_file.display());
    }

    prepare_output(path)?;

    duct::cmd(
        "qemu-img",
        [
            OsString::from("create"),
            OsString::from("-f"),
            OsString::from(DiskFormat::Qcow2.as_qemu_arg()),
            OsString::from("-F"),
            OsString::from(backing_format.as_qemu_arg()),
            OsString::from("-b"),
            backing_file.as_os_str().to_os_string(),
            path.as_os_str().to_os_string(),
        ],
    )
    .run()
    .with_context(|| format!("failed to run qemu-img for {}", path.display()))?;

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

#[derive(Debug, Deserialize, Eq, PartialEq)]
struct QemuImgInfo {
    format: Option<String>,
    #[serde(rename = "virtual-size")]
    virtual_size: Option<u64>,
    #[serde(rename = "actual-size")]
    actual_size: Option<u64>,
    #[serde(rename = "backing-filename")]
    backing_filename: Option<String>,
    #[serde(rename = "backing-filename-format")]
    backing_filename_format: Option<String>,
}

fn parse_qemu_img_info(output: &str) -> Result<QemuImgInfo> {
    serde_json::from_str(output).context("invalid qemu-img JSON output")
}

fn format_optional_bytes(bytes: Option<u64>) -> String {
    bytes.map(format_bytes).unwrap_or_else(|| "-".to_string())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    let value = format!("{value:.1}").trim_end_matches(".0").to_string();
    format!("{value} {}", UNITS[unit])
}

impl DiskFormat {
    pub fn as_qemu_arg(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Qcow2 => "qcow2",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qemu_img_info_json() {
        let output = r#"{
  "virtual-size": 42949672960,
  "filename": ".tmp/disks/install-os.qcow2",
  "cluster-size": 65536,
  "format": "qcow2",
  "actual-size": 200704,
  "backing-filename": "base.qcow2",
  "backing-filename-format": "qcow2"
}"#;

        assert_eq!(
            parse_qemu_img_info(output).unwrap(),
            QemuImgInfo {
                format: Some("qcow2".to_string()),
                virtual_size: Some(42_949_672_960),
                actual_size: Some(200_704),
                backing_filename: Some("base.qcow2".to_string()),
                backing_filename_format: Some("qcow2".to_string()),
            }
        );
    }

    #[test]
    fn formats_byte_sizes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(200_704), "196 KiB");
        assert_eq!(format_bytes(42_949_672_960), "40 GiB");
        assert_eq!(format_bytes(1_610_612_736), "1.5 GiB");
    }
}
