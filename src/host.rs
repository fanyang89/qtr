use std::{
    env, fs,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

use crate::config::{HostArgs, HostCommand, SetupLibvirtAccessArgs};

const LIBVIRT_MANAGE_ACTION: &str = "org.libvirt.unix.manage";

pub fn run(args: HostArgs) -> Result<()> {
    match args.command {
        HostCommand::SetupLibvirtAccess(args) => setup_libvirt_access(args),
    }
}

fn setup_libvirt_access(args: SetupLibvirtAccessArgs) -> Result<()> {
    let user = resolve_target_user(args.user.as_deref())?;

    require_root(args.dry_run)?;
    ensure_user_exists(&user)?;
    ensure_group_exists(&args.group)?;

    let rule = build_polkit_rule(&args.group);

    if args.dry_run {
        println!("would add user {user} to group {}", args.group);
        println!("would write {}:", args.rule_path.display());
        print!("{rule}");
        return Ok(());
    }

    add_user_to_group(&user, &args.group)?;
    write_polkit_rule(&args.rule_path, &rule)?;

    eprintln!("[qtr] added {user} to group {}", args.group);
    eprintln!("[qtr] wrote {}", args.rule_path.display());
    eprintln!("[qtr] re-login or run: newgrp {}", args.group);

    Ok(())
}

fn resolve_target_user(user: Option<&str>) -> Result<String> {
    if let Some(user) = user.filter(|value| !value.is_empty()) {
        return Ok(user.to_string());
    }

    if let Ok(user) = env::var("SUDO_USER") {
        if !user.is_empty() {
            return Ok(user);
        }
    }

    env::var("USER").context("failed to determine target user; pass --user")
}

fn require_root(dry_run: bool) -> Result<()> {
    if dry_run {
        return Ok(());
    }

    let uid = command_stdout(Command::new("id").arg("-u"))?;
    if uid.trim() != "0" {
        bail!("host setup requires root; run with sudo");
    }

    Ok(())
}

fn ensure_user_exists(user: &str) -> Result<()> {
    run_status(Command::new("id").arg("-u").arg(user))
        .with_context(|| format!("user {user} does not exist"))
}

fn ensure_group_exists(group: &str) -> Result<()> {
    run_status(Command::new("getent").arg("group").arg(group))
        .with_context(|| format!("group {group} does not exist"))
}

fn add_user_to_group(user: &str, group: &str) -> Result<()> {
    run_status(Command::new("usermod").arg("-aG").arg(group).arg(user))
        .with_context(|| format!("failed to add user {user} to group {group}"))
}

fn write_polkit_rule(path: &Path, rule: &str) -> Result<()> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    fs::write(path, rule).with_context(|| format!("failed to write {}", path.display()))
}

fn build_polkit_rule(group: &str) -> String {
    format!(
        r#"polkit.addRule(function(action, subject) {{
  if (action.id == "{action}" &&
      subject.isInGroup("{group}")) {{
    return polkit.Result.YES;
  }}
}});
"#,
        action = LIBVIRT_MANAGE_ACTION,
        group = escape_js_string(group),
    )
}

fn run_status(command: &mut Command) -> Result<()> {
    let status = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to run {}", command_name(command)))?;
    if !status.success() {
        bail!("{} exited with {status}", command_name(command));
    }

    Ok(())
}

fn command_stdout(command: &mut Command) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("failed to run {}", command_name(command)))?;
    if !output.status.success() {
        bail!("{} exited with {}", command_name(command), output.status);
    }

    String::from_utf8(output.stdout).context("command output was not UTF-8")
}

fn command_name(command: &Command) -> String {
    command.get_program().to_string_lossy().into_owned()
}

fn escape_js_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
