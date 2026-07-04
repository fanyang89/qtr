use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::json;
use virt::{domain::Domain, sys};

#[derive(Debug)]
pub struct GuestExecResult {
    pub exitcode: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct GuestExecStartResponse {
    #[serde(rename = "return")]
    result: GuestExecStartReturn,
}

#[derive(Debug, Deserialize)]
struct GuestExecStartReturn {
    pid: i64,
}

#[derive(Debug, Deserialize)]
struct GuestExecStatusResponse {
    #[serde(rename = "return")]
    result: GuestExecStatusReturn,
}

#[derive(Debug, Deserialize)]
struct GuestExecStatusReturn {
    exited: bool,
    exitcode: Option<i32>,
    signal: Option<i32>,
    #[serde(rename = "out-data")]
    out_data: Option<String>,
    #[serde(rename = "err-data")]
    err_data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GuestFileOpenResponse {
    #[serde(rename = "return")]
    handle: i64,
}

#[derive(Debug, Deserialize)]
struct GuestFileWriteResponse {
    #[serde(rename = "return")]
    result: GuestFileWriteReturn,
}

#[derive(Debug, Deserialize)]
struct GuestFileWriteReturn {
    count: i64,
}

#[derive(Debug, Deserialize)]
struct GuestFileReadResponse {
    #[serde(rename = "return")]
    result: GuestFileReadReturn,
}

#[derive(Debug, Deserialize)]
struct GuestFileReadReturn {
    count: i64,
    #[serde(default, rename = "buf-b64")]
    buf_b64: String,
    eof: bool,
}

#[derive(Debug, Serialize)]
struct GuestExecArgs<'a> {
    path: &'a str,
    arg: Vec<&'a str>,
    #[serde(rename = "capture-output")]
    capture_output: bool,
}

pub fn wait_ready(domain: &Domain, timeout: Duration) -> Result<()> {
    let started = Instant::now();

    loop {
        if send_command(domain, r#"{"execute":"guest-ping"}"#).is_ok() {
            return Ok(());
        }

        if started.elapsed() >= timeout {
            bail!("timed out waiting for qemu guest agent");
        }

        thread::sleep(Duration::from_secs(1));
    }
}

pub fn run_command(domain: &Domain, command: &str, timeout: Duration) -> Result<GuestExecResult> {
    let args = GuestExecArgs {
        path: "/bin/sh",
        arg: vec!["-lc", command],
        capture_output: true,
    };
    let request = json!({
        "execute": "guest-exec",
        "arguments": args,
    });
    let response =
        send_command(domain, &request.to_string()).context("failed to start guest command")?;
    let start: GuestExecStartResponse = serde_json::from_str(&response)
        .with_context(|| format!("failed to parse guest-exec response: {response}"))?;

    wait_exec_status(domain, start.result.pid, timeout)
}

pub fn write_file(domain: &Domain, path: &str, contents: &[u8]) -> Result<()> {
    let handle = open_file(domain, path, "w")?;
    let write_result = (|| {
        for chunk in contents.chunks(48 * 1024) {
            write_file_chunk(domain, handle, chunk)?;
        }
        flush_file(domain, handle)
    })();
    let close_result = close_file(domain, handle);

    write_result.and(close_result)
}

pub fn read_file(domain: &Domain, path: &str) -> Result<Vec<u8>> {
    let handle = open_file(domain, path, "r")?;
    let read_result: Result<Vec<u8>> = (|| {
        let mut contents = Vec::new();
        loop {
            let chunk = read_file_chunk(domain, handle, 48 * 1024)?;
            contents.extend_from_slice(&chunk.data);
            if chunk.eof {
                break;
            }
        }
        Ok(contents)
    })();
    let close_result = close_file(domain, handle);
    let contents = read_result?;
    close_result?;

    Ok(contents)
}

struct GuestFileChunk {
    data: Vec<u8>,
    eof: bool,
}

fn open_file(domain: &Domain, path: &str, mode: &str) -> Result<i64> {
    let request = json!({
        "execute": "guest-file-open",
        "arguments": {
            "path": path,
            "mode": mode,
        },
    });
    let response = send_command(domain, &request.to_string())
        .with_context(|| format!("failed to open guest file {path}"))?;
    let opened: GuestFileOpenResponse = serde_json::from_str(&response)
        .with_context(|| format!("failed to parse guest-file-open response: {response}"))?;

    Ok(opened.handle)
}

fn write_file_chunk(domain: &Domain, handle: i64, chunk: &[u8]) -> Result<()> {
    let encoded = STANDARD.encode(chunk);
    let request = json!({
        "execute": "guest-file-write",
        "arguments": {
            "handle": handle,
            "buf-b64": encoded,
        },
    });
    let response = send_command(domain, &request.to_string())
        .with_context(|| format!("failed to write guest file handle {handle}"))?;
    let written: GuestFileWriteResponse = serde_json::from_str(&response)
        .with_context(|| format!("failed to parse guest-file-write response: {response}"))?;
    if written.result.count != chunk.len() as i64 {
        bail!(
            "short guest file write: wrote {} of {} bytes",
            written.result.count,
            chunk.len()
        );
    }

    Ok(())
}

fn read_file_chunk(domain: &Domain, handle: i64, count: i64) -> Result<GuestFileChunk> {
    let request = json!({
        "execute": "guest-file-read",
        "arguments": {
            "handle": handle,
            "count": count,
        },
    });
    let response = send_command(domain, &request.to_string())
        .with_context(|| format!("failed to read guest file handle {handle}"))?;
    let read: GuestFileReadResponse = serde_json::from_str(&response)
        .with_context(|| format!("failed to parse guest-file-read response: {response}"))?;
    let data = STANDARD
        .decode(read.result.buf_b64)
        .context("failed to decode guest file read buffer")?;
    if read.result.count != data.len() as i64 {
        bail!(
            "guest file read count mismatch: response count {} decoded {} bytes",
            read.result.count,
            data.len()
        );
    }

    Ok(GuestFileChunk {
        data,
        eof: read.result.eof,
    })
}

fn flush_file(domain: &Domain, handle: i64) -> Result<()> {
    let request = json!({
        "execute": "guest-file-flush",
        "arguments": { "handle": handle },
    });
    send_command(domain, &request.to_string())
        .with_context(|| format!("failed to flush guest file handle {handle}"))?;

    Ok(())
}

fn close_file(domain: &Domain, handle: i64) -> Result<()> {
    let request = json!({
        "execute": "guest-file-close",
        "arguments": { "handle": handle },
    });
    send_command(domain, &request.to_string())
        .with_context(|| format!("failed to close guest file handle {handle}"))?;

    Ok(())
}

fn wait_exec_status(domain: &Domain, pid: i64, timeout: Duration) -> Result<GuestExecResult> {
    let started = Instant::now();

    loop {
        let request = json!({
            "execute": "guest-exec-status",
            "arguments": { "pid": pid },
        });
        let response = send_command(domain, &request.to_string())
            .with_context(|| format!("failed to query guest command pid {pid}"))?;
        let status: GuestExecStatusResponse = serde_json::from_str(&response)
            .with_context(|| format!("failed to parse guest-exec-status response: {response}"))?;

        if status.result.exited {
            let exitcode = match (status.result.exitcode, status.result.signal) {
                (Some(code), _) => code,
                (None, Some(signal)) => 128 + signal,
                (None, None) => return Err(anyhow!("guest command exited without exit code")),
            };
            let stdout = decode_output(status.result.out_data.as_deref(), "stdout")?;
            let stderr = decode_output(status.result.err_data.as_deref(), "stderr")?;

            return Ok(GuestExecResult {
                exitcode,
                stdout,
                stderr,
            });
        }

        if started.elapsed() >= timeout {
            bail!("timed out waiting for guest command pid {pid}");
        }

        thread::sleep(Duration::from_secs(1));
    }
}

fn decode_output(data: Option<&str>, stream_name: &str) -> Result<Vec<u8>> {
    match data {
        Some(value) => STANDARD
            .decode(value)
            .with_context(|| format!("failed to decode guest {stream_name}")),
        None => Ok(Vec::new()),
    }
}

fn send_command(domain: &Domain, command: &str) -> Result<String> {
    domain
        .qemu_agent_command(
            command,
            sys::VIR_DOMAIN_QEMU_AGENT_COMMAND_DEFAULT as i32,
            0,
        )
        .map_err(|err| anyhow!(err))
}
