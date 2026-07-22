use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::json;
use virt::domain::Domain;

#[derive(Debug)]
pub struct GuestExecResult {
    pub exitcode: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug)]
pub struct GuestExecChild {
    pub pid: i64,
}

#[derive(Debug)]
pub struct GuestExecStatus {
    pub exited: bool,
    pub exitcode: Option<i32>,
}

#[derive(Clone, Copy, Debug)]
pub struct GuestAgentDeadline {
    started: Instant,
    timeout: Duration,
}

impl GuestAgentDeadline {
    pub fn new(timeout: Duration) -> Self {
        Self {
            started: Instant::now(),
            timeout,
        }
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.timeout
            .checked_sub(self.started.elapsed())
            .filter(|remaining| !remaining.is_zero())
    }

    pub fn expired(&self) -> bool {
        self.remaining().is_none()
    }
}

pub struct GuestFileChunk {
    pub data: Vec<u8>,
    pub eof: bool,
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
struct GuestInfoResponse {
    #[serde(rename = "return")]
    result: GuestInfoReturn,
}

#[derive(Debug, Deserialize)]
struct GuestInfoReturn {
    supported_commands: Vec<GuestAgentCommandInfo>,
}

#[derive(Debug, Deserialize)]
struct GuestAgentCommandInfo {
    name: String,
    enabled: bool,
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

#[derive(Debug, Deserialize)]
struct GuestFileSeekResponse {
    #[serde(rename = "return")]
    result: GuestFileSeekReturn,
}

#[derive(Debug, Deserialize)]
struct GuestFileSeekReturn {
    position: i64,
}

#[derive(Debug, Serialize)]
struct GuestExecArgs<'a> {
    path: &'a str,
    arg: Vec<&'a str>,
    #[serde(rename = "capture-output")]
    capture_output: bool,
}

pub fn wait_ready(domain: &Domain, timeout: Duration) -> Result<()> {
    let deadline = GuestAgentDeadline::new(timeout);

    loop {
        let Some(remaining) = deadline.remaining() else {
            bail!("timed out waiting for qemu guest agent");
        };

        if send_command(domain, r#"{"execute":"guest-ping"}"#, remaining).is_ok() {
            return Ok(());
        }

        thread::sleep(remaining.min(Duration::from_secs(1)));
    }
}

pub fn run_command(domain: &Domain, command: &str, timeout: Duration) -> Result<GuestExecResult> {
    let deadline = GuestAgentDeadline::new(timeout);
    let child = start_command(domain, command, true, &deadline)?;

    wait_exec_status(domain, child.pid, &deadline)
}

pub fn start_command(
    domain: &Domain,
    command: &str,
    capture_output: bool,
    deadline: &GuestAgentDeadline,
) -> Result<GuestExecChild> {
    if matches!(
        guest_command_enabled(domain, "guest-exec", deadline),
        Ok(Some(false))
    ) {
        bail!(guest_exec_disabled_message());
    }

    let args = GuestExecArgs {
        path: "/bin/sh",
        arg: vec!["-lc", command],
        capture_output,
    };
    let request = json!({
        "execute": "guest-exec",
        "arguments": args,
    });
    let response = match send_deadline_command(domain, &request.to_string(), deadline) {
        Ok(response) => response,
        Err(err) if is_guest_exec_disabled_error(&err) => {
            return Err(err).context(guest_exec_disabled_message());
        }
        Err(err) => return Err(err).context("failed to start guest command"),
    };
    let start: GuestExecStartResponse = serde_json::from_str(&response)
        .with_context(|| format!("failed to parse guest-exec response: {response}"))?;

    Ok(GuestExecChild {
        pid: start.result.pid,
    })
}

fn guest_command_enabled(
    domain: &Domain,
    command: &str,
    deadline: &GuestAgentDeadline,
) -> Result<Option<bool>> {
    let response = send_deadline_command(domain, r#"{"execute":"guest-info"}"#, deadline)
        .context("failed to query guest agent command list")?;
    let info: GuestInfoResponse = serde_json::from_str(&response)
        .with_context(|| format!("failed to parse guest-info response: {response}"))?;

    Ok(info
        .result
        .supported_commands
        .into_iter()
        .find(|supported| supported.name == command)
        .map(|supported| supported.enabled))
}

fn is_guest_exec_disabled_error(err: &anyhow::Error) -> bool {
    let message = err.to_string();
    message.contains("guest-exec has been disabled")
        || message.contains("command guest-exec has been disabled")
}

fn guest_exec_disabled_message() -> &'static str {
    "guest-exec is disabled by qemu-guest-agent inside the guest; enable the guest-exec RPC in qemu-ga block-rpcs/allow-rpcs configuration and restart qemu-guest-agent"
}

pub fn write_file(domain: &Domain, path: &str, contents: &[u8], timeout: Duration) -> Result<()> {
    let deadline = GuestAgentDeadline::new(timeout);

    write_file_with_deadline(domain, path, contents, &deadline)
}

pub fn write_file_with_deadline(
    domain: &Domain,
    path: &str,
    contents: &[u8],
    deadline: &GuestAgentDeadline,
) -> Result<()> {
    let handle = open_file(domain, path, "w", deadline)?;
    let write_result = (|| {
        for chunk in contents.chunks(48 * 1024) {
            write_file_chunk(domain, handle, chunk, deadline)?;
        }
        flush_file(domain, handle, deadline)
    })();
    let close_result = close_file(domain, handle, deadline);

    write_result.and(close_result)
}

pub fn read_file(domain: &Domain, path: &str, timeout: Duration) -> Result<Vec<u8>> {
    let deadline = GuestAgentDeadline::new(timeout);

    read_file_with_deadline(domain, path, &deadline)
}

pub fn read_file_with_deadline(
    domain: &Domain,
    path: &str,
    deadline: &GuestAgentDeadline,
) -> Result<Vec<u8>> {
    let handle = open_file(domain, path, "r", deadline)?;
    let read_result: Result<Vec<u8>> = (|| {
        let mut contents = Vec::new();
        loop {
            let chunk = read_file_chunk(domain, handle, 48 * 1024, deadline)?;
            contents.extend_from_slice(&chunk.data);
            if chunk.eof {
                break;
            }
        }
        Ok(contents)
    })();
    let close_result = close_file(domain, handle, deadline);
    let contents = read_result?;
    close_result?;

    Ok(contents)
}

pub fn read_file_from(
    domain: &Domain,
    path: &str,
    offset: i64,
    count: i64,
    deadline: &GuestAgentDeadline,
) -> Result<GuestFileChunk> {
    let handle = open_file(domain, path, "r", deadline)?;
    let read_result: Result<GuestFileChunk> = (|| {
        seek_file(domain, handle, offset, deadline)?;
        read_file_chunk(domain, handle, count, deadline)
    })();
    let close_result = close_file(domain, handle, deadline);
    let chunk = read_result?;
    close_result?;

    Ok(chunk)
}

fn open_file(
    domain: &Domain,
    path: &str,
    mode: &str,
    deadline: &GuestAgentDeadline,
) -> Result<i64> {
    let request = json!({
        "execute": "guest-file-open",
        "arguments": {
            "path": path,
            "mode": mode,
        },
    });
    let response = send_deadline_command(domain, &request.to_string(), deadline)
        .with_context(|| format!("failed to open guest file {path}"))?;
    let opened: GuestFileOpenResponse = serde_json::from_str(&response)
        .with_context(|| format!("failed to parse guest-file-open response: {response}"))?;

    Ok(opened.handle)
}

fn write_file_chunk(
    domain: &Domain,
    handle: i64,
    chunk: &[u8],
    deadline: &GuestAgentDeadline,
) -> Result<()> {
    let encoded = STANDARD.encode(chunk);
    let request = json!({
        "execute": "guest-file-write",
        "arguments": {
            "handle": handle,
            "buf-b64": encoded,
        },
    });
    let response = send_deadline_command(domain, &request.to_string(), deadline)
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

fn read_file_chunk(
    domain: &Domain,
    handle: i64,
    count: i64,
    deadline: &GuestAgentDeadline,
) -> Result<GuestFileChunk> {
    let request = json!({
        "execute": "guest-file-read",
        "arguments": {
            "handle": handle,
            "count": count,
        },
    });
    let response = send_deadline_command(domain, &request.to_string(), deadline)
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

fn seek_file(
    domain: &Domain,
    handle: i64,
    offset: i64,
    deadline: &GuestAgentDeadline,
) -> Result<()> {
    let request = json!({
        "execute": "guest-file-seek",
        "arguments": {
            "handle": handle,
            "offset": offset,
            "whence": "set",
        },
    });
    let response = send_deadline_command(domain, &request.to_string(), deadline)
        .with_context(|| format!("failed to seek guest file handle {handle}"))?;
    let seek: GuestFileSeekResponse = serde_json::from_str(&response)
        .with_context(|| format!("failed to parse guest-file-seek response: {response}"))?;
    if seek.result.position != offset {
        bail!(
            "guest file seek landed at {} instead of {}",
            seek.result.position,
            offset
        );
    }

    Ok(())
}

fn flush_file(domain: &Domain, handle: i64, deadline: &GuestAgentDeadline) -> Result<()> {
    let request = json!({
        "execute": "guest-file-flush",
        "arguments": { "handle": handle },
    });
    send_deadline_command(domain, &request.to_string(), deadline)
        .with_context(|| format!("failed to flush guest file handle {handle}"))?;

    Ok(())
}

fn close_file(domain: &Domain, handle: i64, deadline: &GuestAgentDeadline) -> Result<()> {
    let request = json!({
        "execute": "guest-file-close",
        "arguments": { "handle": handle },
    });
    send_deadline_command(domain, &request.to_string(), deadline)
        .with_context(|| format!("failed to close guest file handle {handle}"))?;

    Ok(())
}

fn wait_exec_status(
    domain: &Domain,
    pid: i64,
    deadline: &GuestAgentDeadline,
) -> Result<GuestExecResult> {
    loop {
        if deadline.expired() {
            bail!("timed out waiting for guest command pid {pid}");
        }

        let status = query_raw_exec_status(domain, pid, deadline)?;

        if status.result.exited {
            let exitcode = exec_status_exitcode(&status.result)?;
            let stdout = decode_output(status.result.out_data.as_deref(), "stdout")?;
            let stderr = decode_output(status.result.err_data.as_deref(), "stderr")?;

            return Ok(GuestExecResult {
                exitcode,
                stdout,
                stderr,
            });
        }

        let Some(remaining) = deadline.remaining() else {
            bail!("timed out waiting for guest command pid {pid}");
        };
        thread::sleep(remaining.min(Duration::from_secs(1)));
    }
}

pub fn query_exec_status(
    domain: &Domain,
    pid: i64,
    deadline: &GuestAgentDeadline,
) -> Result<GuestExecStatus> {
    let status = query_raw_exec_status(domain, pid, deadline)?;
    let exitcode = if status.result.exited {
        Some(exec_status_exitcode(&status.result)?)
    } else {
        None
    };

    Ok(GuestExecStatus {
        exited: status.result.exited,
        exitcode,
    })
}

fn query_raw_exec_status(
    domain: &Domain,
    pid: i64,
    deadline: &GuestAgentDeadline,
) -> Result<GuestExecStatusResponse> {
    let request = json!({
        "execute": "guest-exec-status",
        "arguments": { "pid": pid },
    });
    let response = send_deadline_command(domain, &request.to_string(), deadline)
        .with_context(|| format!("failed to query guest command pid {pid}"))?;
    serde_json::from_str(&response)
        .with_context(|| format!("failed to parse guest-exec-status response: {response}"))
}

fn exec_status_exitcode(status: &GuestExecStatusReturn) -> Result<i32> {
    match (status.exitcode, status.signal) {
        (Some(code), _) => Ok(code),
        (None, Some(signal)) => Ok(128 + signal),
        (None, None) => Err(anyhow!("guest command exited without exit code")),
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

fn send_deadline_command(
    domain: &Domain,
    command: &str,
    deadline: &GuestAgentDeadline,
) -> Result<String> {
    let timeout = deadline
        .remaining()
        .context("timed out waiting for qemu guest agent")?;

    send_command(domain, command, timeout)
}

fn send_command(domain: &Domain, command: &str, timeout: Duration) -> Result<String> {
    domain
        .qemu_agent_command(command, agent_command_timeout_secs(timeout), 0)
        .map_err(|err| anyhow!(err))
}

fn agent_command_timeout_secs(timeout: Duration) -> i32 {
    let rounded_secs = timeout
        .as_secs()
        .saturating_add(u64::from(timeout.subsec_nanos() > 0));
    rounded_secs.clamp(1, i32::MAX as u64) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec_status(exitcode: Option<i32>, signal: Option<i32>) -> GuestExecStatusReturn {
        GuestExecStatusReturn {
            exited: true,
            exitcode,
            signal,
            out_data: None,
            err_data: None,
        }
    }

    #[test]
    fn exec_status_prefers_exitcode() {
        assert_eq!(
            exec_status_exitcode(&exec_status(Some(3), None)).unwrap(),
            3
        );
        assert_eq!(
            exec_status_exitcode(&exec_status(Some(0), Some(9))).unwrap(),
            0
        );
    }

    #[test]
    fn exec_status_maps_signal_to_shell_convention() {
        assert_eq!(
            exec_status_exitcode(&exec_status(None, Some(9))).unwrap(),
            137
        );
        assert_eq!(
            exec_status_exitcode(&exec_status(None, Some(15))).unwrap(),
            143
        );
    }

    #[test]
    fn exec_status_rejects_missing_outcome() {
        assert!(exec_status_exitcode(&exec_status(None, None)).is_err());
    }

    #[test]
    fn decode_output_handles_absent_and_valid_data() {
        assert_eq!(decode_output(None, "stdout").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_output(Some("aGVsbG8="), "stdout").unwrap(), b"hello");
        assert!(decode_output(Some("!!!not-base64!!!"), "stderr").is_err());
    }

    #[test]
    fn agent_timeout_rounds_up_and_clamps() {
        assert_eq!(agent_command_timeout_secs(Duration::ZERO), 1);
        assert_eq!(agent_command_timeout_secs(Duration::from_secs(1)), 1);
        assert_eq!(agent_command_timeout_secs(Duration::from_millis(1500)), 2);
        assert_eq!(
            agent_command_timeout_secs(Duration::from_secs(u64::MAX)),
            i32::MAX
        );
    }
}
