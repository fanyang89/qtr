use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Matrix {
    #[serde(rename = "case")]
    pub cases: Vec<TestCase>,
}

#[derive(Debug, Deserialize)]
pub struct TestCase {
    pub name: String,
    pub data_disk_io: DiskIo,
    pub data_disk_cache: DiskCache,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskIo {
    Threads,
    Native,
    IoUring,
}

impl DiskIo {
    pub fn as_xml(self) -> &'static str {
        match self {
            Self::Threads => "threads",
            Self::Native => "native",
            Self::IoUring => "io_uring",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskCache {
    Default,
    None,
    Writethrough,
    Writeback,
    Directsync,
    Unsafe,
}

impl DiskCache {
    pub fn as_xml(self) -> &'static str {
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

pub fn load_matrix(path: &Path) -> Result<Matrix> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read matrix {}", path.display()))?;
    let matrix: Matrix = toml::from_str(&content)
        .with_context(|| format!("failed to parse matrix {}", path.display()))?;

    validate_matrix(&matrix)?;
    Ok(matrix)
}

fn validate_matrix(matrix: &Matrix) -> Result<()> {
    if matrix.cases.is_empty() {
        bail!("matrix must contain at least one [[case]]");
    }

    for case in &matrix.cases {
        if case.name.is_empty() {
            bail!("case name must not be empty");
        }

        if !case
            .name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            bail!(
                "case name {:?} may only contain ASCII letters, digits, '-', '_' and '.'",
                case.name
            );
        }
    }

    Ok(())
}
