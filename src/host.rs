use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
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
    let qemu_acl_dirs = qemu_acl_dirs(&args)?;

    require_root(args.dry_run)?;
    ensure_user_exists(&user)?;
    ensure_group_exists(&args.group)?;
    if !qemu_acl_dirs.is_empty() {
        ensure_user_exists(&args.qemu_user)?;
        ensure_setfacl_exists()?;
    }

    let rule = build_polkit_rule(&args.group);

    if args.dry_run {
        println!("would add user {user} to group {}", args.group);
        println!("would write {}:", args.rule_path.display());
        print!("{rule}");
        print_qemu_acl_plan(&args.qemu_user, &qemu_acl_dirs)?;
        return Ok(());
    }

    add_user_to_group(&user, &args.group)?;
    write_polkit_rule(&args.rule_path, &rule)?;
    apply_qemu_acls(&args.qemu_user, &qemu_acl_dirs)?;

    eprintln!("[qtr] added {user} to group {}", args.group);
    eprintln!("[qtr] wrote {}", args.rule_path.display());
    for dir in &qemu_acl_dirs {
        eprintln!(
            "[qtr] granted qemu {} access: {}",
            dir.access.description(),
            dir.path.display()
        );
    }
    eprintln!("[qtr] re-login or run: newgrp {}", args.group);

    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QemuDirAccess {
    ReadWrite,
    ReadOnly,
}

impl QemuDirAccess {
    fn directory_acl(self) -> &'static str {
        match self {
            Self::ReadWrite => "rwx",
            Self::ReadOnly => "r-x",
        }
    }

    fn file_acl(self) -> &'static str {
        match self {
            Self::ReadWrite => "rw-",
            Self::ReadOnly => "r--",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::ReadWrite => "read-write",
            Self::ReadOnly => "read-only",
        }
    }
}

#[derive(Debug)]
struct QemuAclDir {
    path: PathBuf,
    access: QemuDirAccess,
}

fn qemu_acl_dirs(args: &SetupLibvirtAccessArgs) -> Result<Vec<QemuAclDir>> {
    let mut dirs = Vec::new();
    for path in &args.qemu_rw_dir {
        dirs.push(QemuAclDir {
            path: canonical_dir(path)?,
            access: QemuDirAccess::ReadWrite,
        });
    }
    for path in &args.qemu_ro_dir {
        dirs.push(QemuAclDir {
            path: canonical_dir(path)?,
            access: QemuDirAccess::ReadOnly,
        });
    }

    ensure_non_overlapping_acl_dirs(&dirs)?;
    Ok(dirs)
}

fn canonical_dir(path: &Path) -> Result<PathBuf> {
    let absolute = absolute_path(path)?;
    let canonical = fs::canonicalize(&absolute)
        .with_context(|| format!("failed to resolve directory {}", absolute.display()))?;
    if !canonical.is_dir() {
        bail!("{} is not a directory", canonical.display());
    }

    Ok(canonical)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    Ok(env::current_dir()
        .context("failed to determine current directory")?
        .join(path))
}

fn ensure_non_overlapping_acl_dirs(dirs: &[QemuAclDir]) -> Result<()> {
    for (index, left) in dirs.iter().enumerate() {
        for right in dirs.iter().skip(index + 1) {
            if left.path == right.path {
                if left.access != right.access {
                    bail!(
                        "{} was requested with both read-write and read-only qemu access",
                        left.path.display()
                    );
                }
                continue;
            }

            if left.path.starts_with(&right.path) || right.path.starts_with(&left.path) {
                bail!(
                    "qemu ACL directories must not overlap: {} and {}",
                    left.path.display(),
                    right.path.display()
                );
            }
        }
    }

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

fn ensure_setfacl_exists() -> Result<()> {
    run_status(Command::new("setfacl").arg("--version")).context("setfacl is required")
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

fn print_qemu_acl_plan(user: &str, dirs: &[QemuAclDir]) -> Result<()> {
    for ancestor in qemu_acl_ancestors(dirs) {
        print_setfacl_command(user, "--x", &ancestor, false);
    }
    for dir in dirs {
        print_qemu_acl_tree(user, dir.access, &dir.path)?;
    }

    Ok(())
}

fn apply_qemu_acls(user: &str, dirs: &[QemuAclDir]) -> Result<()> {
    for ancestor in qemu_acl_ancestors(dirs) {
        set_user_acl(user, "--x", &ancestor, false)?;
    }
    for dir in dirs {
        apply_qemu_acl_tree(user, dir.access, &dir.path)?;
    }

    Ok(())
}

fn qemu_acl_ancestors(dirs: &[QemuAclDir]) -> BTreeSet<PathBuf> {
    let mut ancestors = BTreeSet::new();
    for dir in dirs {
        let mut current = dir.path.parent();
        while let Some(path) = current {
            if path.parent().is_none() {
                break;
            }
            ancestors.insert(path.to_path_buf());
            current = path.parent();
        }
    }

    ancestors
}

fn print_qemu_acl_tree(user: &str, access: QemuDirAccess, path: &Path) -> Result<()> {
    let file_type = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .file_type();

    if file_type.is_dir() {
        print_setfacl_command(user, access.directory_acl(), path, false);
        print_setfacl_command(user, access.directory_acl(), path, true);
        for entry in
            fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to read entry in {}", path.display()))?;
            print_qemu_acl_tree(user, access, &entry.path())?;
        }
    } else if file_type.is_file() {
        print_setfacl_command(user, access.file_acl(), path, false);
    }

    Ok(())
}

fn apply_qemu_acl_tree(user: &str, access: QemuDirAccess, path: &Path) -> Result<()> {
    let file_type = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .file_type();

    if file_type.is_dir() {
        set_user_acl(user, access.directory_acl(), path, false)?;
        set_user_acl(user, access.directory_acl(), path, true)?;
        for entry in
            fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to read entry in {}", path.display()))?;
            apply_qemu_acl_tree(user, access, &entry.path())?;
        }
    } else if file_type.is_file() {
        set_user_acl(user, access.file_acl(), path, false)?;
    }

    Ok(())
}

fn print_setfacl_command(user: &str, perms: &str, path: &Path, default_acl: bool) {
    let entry = acl_entry(user, perms, default_acl);
    println!("would run: setfacl -m {entry} {}", path.display());
}

fn set_user_acl(user: &str, perms: &str, path: &Path, default_acl: bool) -> Result<()> {
    let entry = acl_entry(user, perms, default_acl);
    run_status(Command::new("setfacl").arg("-m").arg(entry).arg(path))
        .with_context(|| format!("failed to set qemu ACL on {}", path.display()))
}

fn acl_entry(user: &str, perms: &str, default_acl: bool) -> String {
    if default_acl {
        format!("d:u:{user}:{perms}")
    } else {
        format!("u:{user}:{perms}")
    }
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
