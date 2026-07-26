use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result};

fn main() -> Result<()> {
    let output = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("usage: cargo run --example export-openapi -- OUTPUT")?;
    let document = serde_json::to_string_pretty(&qtr::web::openapi_document())
        .context("failed to serialize OpenAPI document")?;
    fs::write(&output, format!("{document}\n"))
        .with_context(|| format!("failed to write {}", output.display()))
}
