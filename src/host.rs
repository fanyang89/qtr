use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use duct::cmd;

use crate::{
    config::{FixVmPermsArgs, HostArgs, HostCommand, SetupLibvirtAccessArgs},
    vm::{VmDiskType, parse_manifest_yaml},
};

const LIBVIRT_MANAGE_ACTION: &str = "org.libvirt.unix.manage";

pub fn run(args: HostArgs) -> Result<()> {
    match args.command {
        HostCommand::SetupLibvirtAccess(args) => setup_libvirt_access(args),
        HostCommand::FixVmPerms(args) => fix_vm_perms(args),
    }
}

fn fix_vm_perms(args: FixVmPermsArgs) -> Result<()> {
    let plan = vm_perms_plan(&args.file)?;

    require_root(args.dry_run)?;
    ensure_user_exists(&args.qemu_user)?;
    ensure_setfacl_exists()?;

    if args.dry_run {
        print_vm_perms_plan(&args.qemu_user, &plan);
        return Ok(());
    }

    apply_vm_perms_plan(&args.qemu_user, &plan)?;

    eprintln!(
        "[qtr] granted qemu access for VM definition {}",
        plan.manifest_path.display()
    );

    Ok(())
}

#[derive(Debug)]
struct VmPermsPlan {
    manifest_path: PathBuf,
    writable_dirs: BTreeSet<PathBuf>,
    read_write_files: BTreeSet<PathBuf>,
    read_only_files: BTreeSet<PathBuf>,
}

fn vm_perms_plan(file: &Path) -> Result<VmPermsPlan> {
    let manifest_path = absolute_path(file)?;
    let manifest_dir = manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let manifest_text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read VM definition {}", manifest_path.display()))?;
    let manifest = parse_manifest_yaml(&manifest_text)
        .with_context(|| format!("failed to parse VM definition {}", manifest_path.display()))?;

    let mut plan = VmPermsPlan {
        manifest_path,
        writable_dirs: BTreeSet::new(),
        read_write_files: BTreeSet::new(),
        read_only_files: BTreeSet::new(),
    };

    for disk in manifest.disks.iter().filter_map(|entry| entry.as_present()) {
        if disk.disk_type == VmDiskType::File {
            let path = manifest_path_ref(&manifest_dir, &disk.path);
            ensure_regular_file(&path, "disk")?;
            plan.read_write_files.insert(path);
        }
    }

    if let Some(cdrom) = &manifest.cdrom {
        let path = manifest_path_ref(&manifest_dir, cdrom);
        ensure_regular_file(&path, "cdrom ISO")?;
        plan.read_only_files.insert(path);
    }
    if let Some(cdroms) = &manifest.cdroms {
        for media in cdroms
            .iter()
            .filter_map(|entry| entry.as_present())
            .filter_map(|cdrom| cdrom.media.as_deref())
        {
            let path = manifest_path_ref(&manifest_dir, media);
            ensure_regular_file(&path, "cdrom ISO")?;
            plan.read_only_files.insert(path);
        }
    }

    if let Some(serial_log) = &manifest.serial_log {
        let serial_log = manifest_path_ref(&manifest_dir, serial_log);
        if let Some(parent) = serial_log
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            plan.writable_dirs.insert(parent.to_path_buf());
        }
        if serial_log.exists() {
            ensure_regular_file(&serial_log, "serial log")?;
            plan.read_write_files.insert(serial_log);
        }
    }

    for path in &plan.read_write_files {
        plan.read_only_files.remove(path);
    }

    Ok(plan)
}

fn manifest_path_ref(base_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn ensure_regular_file(path: &Path, kind: &str) -> Result<()> {
    if !path.exists() {
        bail!("{kind} {} does not exist", path.display());
    }
    if !path.is_file() {
        bail!("{kind} {} is not a regular file", path.display());
    }

    Ok(())
}

fn print_vm_perms_plan(user: &str, plan: &VmPermsPlan) {
    for dir in &plan.writable_dirs {
        if !dir.exists() {
            println!("would create directory: {}", dir.display());
        }
        print_path_access_plan(user, dir, "rwx", true);
        print_setfacl_command(user, "rwx", dir, true);
    }
    for path in &plan.read_write_files {
        print_path_access_plan(user, path, "rw-", false);
    }
    for path in &plan.read_only_files {
        print_path_access_plan(user, path, "r--", false);
    }
}

fn apply_vm_perms_plan(user: &str, plan: &VmPermsPlan) -> Result<()> {
    for dir in &plan.writable_dirs {
        fs::create_dir_all(dir)
            .with_context(|| format!("failed to create directory {}", dir.display()))?;
        apply_path_access(user, dir, "rwx", true)?;
        set_user_acl(user, "rwx", dir, true)?;
    }
    for path in &plan.read_write_files {
        apply_path_access(user, path, "rw-", false)?;
    }
    for path in &plan.read_only_files {
        apply_path_access(user, path, "r--", false)?;
    }

    Ok(())
}

fn print_path_access_plan(user: &str, path: &Path, perms: &str, directory: bool) {
    for ancestor in path_acl_ancestors(path, directory) {
        print_setfacl_command(user, "--x", &ancestor, false);
    }
    print_setfacl_command(user, perms, path, false);
}

fn apply_path_access(user: &str, path: &Path, perms: &str, directory: bool) -> Result<()> {
    for ancestor in path_acl_ancestors(path, directory) {
        set_user_acl(user, "--x", &ancestor, false)?;
    }
    set_user_acl(user, perms, path, false)
}

fn path_acl_ancestors(path: &Path, directory: bool) -> BTreeSet<PathBuf> {
    let mut ancestors = BTreeSet::new();
    let mut current = if directory {
        path.parent()
    } else {
        path.parent().or(Some(path))
    };

    while let Some(path) = current {
        if path.parent().is_none() {
            break;
        }
        ancestors.insert(path.to_path_buf());
        current = path.parent();
    }

    ancestors
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

    if let Ok(user) = env::var("SUDO_USER")
        && !user.is_empty()
    {
        return Ok(user);
    }

    env::var("USER").context("failed to determine target user; pass --user")
}

fn require_root(dry_run: bool) -> Result<()> {
    if dry_run {
        return Ok(());
    }

    if !nix::unistd::geteuid().is_root() {
        bail!("host setup requires root; run with sudo");
    }

    Ok(())
}

fn ensure_user_exists(user: &str) -> Result<()> {
    cmd!("id", "-u", user)
        .stdout_null()
        .stderr_null()
        .run()
        .with_context(|| format!("user {user} does not exist"))?;

    Ok(())
}

fn ensure_group_exists(group: &str) -> Result<()> {
    cmd!("getent", "group", group)
        .stdout_null()
        .stderr_null()
        .run()
        .with_context(|| format!("group {group} does not exist"))?;

    Ok(())
}

fn ensure_setfacl_exists() -> Result<()> {
    which::which("setfacl")
        .map(|_| ())
        .context("setfacl is required")
}

fn add_user_to_group(user: &str, group: &str) -> Result<()> {
    cmd!("usermod", "-aG", group, user)
        .stdout_null()
        .stderr_null()
        .run()
        .with_context(|| format!("failed to add user {user} to group {group}"))?;

    Ok(())
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
    duct::cmd(
        "setfacl",
        ["-m".into(), entry.into(), path.as_os_str().to_os_string()],
    )
    .stdout_null()
    .stderr_null()
    .run()
    .with_context(|| format!("failed to set qemu ACL on {}", path.display()))?;

    Ok(())
}

fn acl_entry(user: &str, perms: &str, default_acl: bool) -> String {
    if default_acl {
        format!("d:u:{user}:{perms}")
    } else {
        format!("u:{user}:{perms}")
    }
}

fn build_polkit_rule(group: &str) -> String {
    let action = serde_json::to_string(LIBVIRT_MANAGE_ACTION)
        .expect("libvirt action should serialize as JSON string");
    let group = serde_json::to_string(group).expect("group should serialize as JSON string");

    format!(
        r#"polkit.addRule(function(action, subject) {{
  if (action.id == {action} &&
      subject.isInGroup({group})) {{
    return polkit.Result.YES;
  }}
}});
"#,
        action = action,
        group = group,
    )
}
