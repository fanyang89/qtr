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
