use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, IsTerminal, Write},
    net::IpAddr,
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use roxmltree::{Document, Node};
use serde::{Deserialize, Serialize};
use similar::TextDiff;
use virt::{connect::Connect, domain::Domain, error::clear_error_callback, sys};

use crate::{
    config::{
        ColorMode, DiskFormat, GraphicsMode, VmApplyArgs, VmArgs, VmCommand, VmCreateArgs,
        VmDumpArgs, VmExecArgs, VmLaunchArgs, VmListArgs, VmNameArgs, VmShutdownArgs,
    },
    disk,
    domain_xml::{self, BootDevice, GraphicsSpec, VmLaunchDomainSpec, build_vm_launch_domain_xml},
    guest_agent,
};

pub fn run(args: VmArgs) -> Result<()> {
    clear_error_callback();

    match args.command {
        VmCommand::Apply(args) => apply(args),
        VmCommand::Dump(args) => dump(args),
        VmCommand::List(args) => list(args),
        VmCommand::Create(args) => create(args).map(|_| ()),
        VmCommand::Launch(args) => launch(args),
        VmCommand::Start(args) => start(args),
        VmCommand::Vnc(args) => vnc(args),
        VmCommand::Exec(args) => exec(args),
        VmCommand::WaitShutdown(args) => wait_shutdown_command(args),
        VmCommand::Shutdown(args) => shutdown(args),
        VmCommand::Destroy(args) => destroy(args),
        VmCommand::Undefine(args) => undefine(args),
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VmManifest {
    name: String,
    system_disk: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    cdrom: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    boot: Option<Vec<String>>,
    #[serde(default = "default_vm_memory_gib", rename = "memoryGiB")]
    memory_gib: u64,
    #[serde(default = "default_vm_vcpus")]
    vcpus: u32,
    #[serde(default = "default_vm_network")]
    network: String,
    #[serde(default = "default_vm_graphics")]
    graphics: GraphicsMode,
    #[serde(default = "default_vm_vnc_listen")]
    vnc_listen: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    vnc_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    serial_log: Option<PathBuf>,
}

fn default_vm_memory_gib() -> u64 {
    4
}

fn default_vm_vcpus() -> u32 {
    2
}

fn default_vm_network() -> String {
    "default".to_string()
}

fn default_vm_graphics() -> GraphicsMode {
    GraphicsMode::Vnc
}

fn default_vm_vnc_listen() -> String {
    "127.0.0.1".to_string()
}

fn apply(args: VmApplyArgs) -> Result<()> {
    let manifest_path = absolute_path(&args.file)?;
    let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let manifest_text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read VM definition {}", manifest_path.display()))?;
    let mut manifest: VmManifest = serde_yaml::from_str(&manifest_text)
        .with_context(|| format!("failed to parse VM definition {}", manifest_path.display()))?;

    normalize_manifest_paths(&mut manifest, manifest_dir)?;
    validate_manifest(&manifest)?;

    let boot = manifest_boot_order(&manifest);
    let boot_devices = domain_xml::parse_boot_devices(&boot)?;
    if boot_devices.contains(&BootDevice::Cdrom) && manifest.cdrom.is_none() {
        bail!("boot order contains cdrom but cdrom was not provided");
    }

    let memory_mib = manifest
        .memory_gib
        .checked_mul(1024)
        .context("memoryGiB is too large")?;

    let current_xml = current_domain_xml(&args.connect_uri, &manifest.name)?;
    let xml = if current_xml.is_empty() {
        build_manifest_domain_xml(&manifest, &boot_devices, memory_mib)
    } else {
        patch_domain_xml(&current_xml, &manifest, &boot_devices, memory_mib)?
    };

    if args.dry_run {
        print_apply_diff(
            &current_xml,
            &manifest.name,
            &manifest_path,
            &xml,
            should_color(args.color),
        );
        return Ok(());
    }

    prepare_serial_log_path(manifest.serial_log.as_deref())?;

    let conn = connect(&args.connect_uri)?;
    let domain = Domain::define_xml_flags(&conn, &xml, sys::VIR_DOMAIN_DEFINE_VALIDATE)
        .with_context(|| format!("failed to apply VM definition {}", manifest.name))?;

    eprintln!("[qtr] applied VM: {}", manifest.name);
    if domain
        .is_active()
        .with_context(|| format!("failed to query domain {} state", manifest.name))?
    {
        eprintln!("[qtr] VM is running; changes apply on next start");
    }

    Ok(())
}

fn build_manifest_domain_xml(
    manifest: &VmManifest,
    boot_devices: &[BootDevice],
    memory_mib: u64,
) -> String {
    build_vm_launch_domain_xml(VmLaunchDomainSpec {
        name: &manifest.name,
        memory_mib,
        vcpus: manifest.vcpus,
        system_disk: &manifest.system_disk,
        cdrom: manifest.cdrom.as_deref(),
        serial_log: manifest.serial_log.as_deref(),
        boot_devices,
        network: &manifest.network,
        graphics: GraphicsSpec {
            mode: manifest.graphics,
            vnc_listen: &manifest.vnc_listen,
            vnc_port: manifest.vnc_port,
        },
    })
}

fn dump(args: VmDumpArgs) -> Result<()> {
    let xml = existing_domain_xml(&args.connect_uri, &args.name)?;
    let manifest = manifest_from_domain_xml(&xml)?;
    let yaml = serde_yaml::to_string(&manifest)
        .with_context(|| format!("failed to serialize VM {} as YAML", args.name))?;

    match args.output {
        Some(path) => {
            if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create directory {}", parent.display()))?;
            }
            fs::write(&path, yaml)
                .with_context(|| format!("failed to write VM YAML {}", path.display()))?;
        }
        None => print!("{yaml}"),
    }

    Ok(())
}

fn manifest_from_domain_xml(xml: &str) -> Result<VmManifest> {
    let doc = Document::parse(xml).context("failed to parse libvirt domain XML")?;
    let domain = doc.root_element();

    let name = required_child_text(domain, "name")?.to_string();
    let memory_mib = memory_mib(domain)?;
    if memory_mib % 1024 != 0 {
        bail!("domain memory {memory_mib} MiB cannot be represented as whole GiB");
    }

    let boot = boot_order(domain)?;
    let devices = required_child(domain, "devices")?;
    let system_disk = disk_source_path(devices, "disk", Some("vda"))?;
    let cdrom = optional_disk_source_path(devices, "cdrom", None)?;
    let network = network_name(devices)?;
    let (graphics, vnc_listen, vnc_port) = graphics_config(devices)?;
    let serial_log = serial_log_path(devices);

    Ok(VmManifest {
        name,
        system_disk,
        cdrom,
        boot: Some(boot),
        memory_gib: memory_mib / 1024,
        vcpus: required_child_text(domain, "vcpu")?
            .parse()
            .context("failed to parse domain vcpus")?,
        network,
        graphics,
        vnc_listen,
        vnc_port,
        serial_log,
    })
}

struct XmlReplacement {
    range: Range<usize>,
    value: String,
}

fn patch_domain_xml(
    xml: &str,
    manifest: &VmManifest,
    boot_devices: &[BootDevice],
    memory_mib: u64,
) -> Result<String> {
    let doc = Document::parse(xml).context("failed to parse existing libvirt domain XML")?;
    let domain = doc.root_element();
    let devices = required_child(domain, "devices")?;
    let mut replacements = Vec::new();

    patch_memory(xml, domain, memory_mib, &mut replacements)?;
    push_text_replacement(
        xml,
        required_child(domain, "vcpu")?,
        &manifest.vcpus.to_string(),
        &mut replacements,
    )?;
    patch_boot_order(xml, domain, boot_devices, &mut replacements)?;
    patch_disk_source(
        xml,
        devices,
        "disk",
        Some("vda"),
        &manifest.system_disk,
        &mut replacements,
    )?;

    if let Some(cdrom) = &manifest.cdrom {
        patch_disk_source(xml, devices, "cdrom", None, cdrom, &mut replacements)?;
    }

    patch_network(xml, devices, &manifest.network, &mut replacements)?;
    patch_graphics(xml, devices, manifest, &mut replacements)?;

    if let Some(serial_log) = &manifest.serial_log {
        patch_serial_log(xml, devices, serial_log, &mut replacements)?;
    }

    Ok(apply_xml_replacements(xml, replacements))
}

fn patch_memory(
    xml: &str,
    domain: Node<'_, '_>,
    memory_mib: u64,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    for tag in ["memory", "currentMemory"] {
        let node = required_child(domain, tag)?;
        let unit = node.attribute("unit").unwrap_or("KiB");
        let value = memory_value_for_unit(memory_mib, unit)?;
        push_text_replacement(xml, node, &value.to_string(), replacements)?;
    }

    Ok(())
}

fn memory_value_for_unit(memory_mib: u64, unit: &str) -> Result<u64> {
    match unit {
        "KiB" => memory_mib
            .checked_mul(1024)
            .context("memoryGiB is too large for KiB domain memory"),
        "MiB" => Ok(memory_mib),
        "GiB" => {
            if memory_mib % 1024 != 0 {
                bail!("memoryGiB cannot be represented as whole GiB in existing domain XML");
            }
            Ok(memory_mib / 1024)
        }
        _ => bail!("unsupported domain memory unit {unit:?}"),
    }
}

fn patch_boot_order(
    xml: &str,
    domain: Node<'_, '_>,
    boot_devices: &[BootDevice],
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let os = required_child(domain, "os")?;
    let boot_nodes = os
        .children()
        .filter(|child| child.has_tag_name("boot"))
        .collect::<Vec<_>>();
    if boot_nodes.is_empty() {
        bail!("cannot update existing domain XML because <os> has no <boot> entries");
    }

    let current_boot = boot_order(domain)?;
    let desired_boot = boot_devices
        .iter()
        .map(|device| boot_device_name(*device).to_string())
        .collect::<Vec<_>>();
    if current_boot == desired_boot {
        return Ok(());
    }

    let first = boot_nodes.first().expect("checked non-empty").range();
    let last = boot_nodes.last().expect("checked non-empty").range();
    let start = line_start(xml, first.start);
    let end = line_end(xml, last.end);
    let indent = &xml[start..first.start];
    let value = boot_devices
        .iter()
        .map(|device| format!("{indent}<boot dev='{}'/>\n", boot_device_name(*device)))
        .collect::<String>();

    replacements.push(XmlReplacement {
        range: start..end,
        value,
    });

    Ok(())
}

fn patch_disk_source(
    xml: &str,
    devices: Node<'_, '_>,
    device: &str,
    target_dev: Option<&str>,
    path: &Path,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let disk = find_disk(devices, device, target_dev)?.with_context(|| {
        let target = target_dev
            .map(|target| format!(" target {target}"))
            .unwrap_or_default();
        format!("cannot update existing domain XML because {device} disk{target} is missing")
    })?;
    let source =
        optional_child(disk, "source").context("domain XML disk is missing source element")?;
    push_attr_replacement(
        xml,
        source,
        "file",
        &path.display().to_string(),
        replacements,
    )
}

fn patch_network(
    xml: &str,
    devices: Node<'_, '_>,
    network: &str,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let interface = devices
        .children()
        .find(|child| child.has_tag_name("interface") && child.attribute("type") == Some("network"))
        .context("cannot update existing domain XML because network interface is missing")?;
    let source = optional_child(interface, "source")
        .context("domain XML network interface is missing source element")?;
    push_attr_replacement(xml, source, "network", network, replacements)
}

fn patch_graphics(
    xml: &str,
    devices: Node<'_, '_>,
    manifest: &VmManifest,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    match manifest.graphics {
        GraphicsMode::None => {
            if devices
                .children()
                .any(|child| child.has_tag_name("graphics"))
            {
                bail!("cannot remove graphics from an existing domain XML yet");
            }
        }
        GraphicsMode::Vnc => {
            let graphics = devices
                .children()
                .find(|child| child.has_tag_name("graphics"))
                .context("cannot update existing domain XML because VNC graphics is missing")?;
            if graphics.attribute("type") != Some("vnc") {
                bail!("cannot update non-VNC graphics in existing domain XML");
            }

            if graphics.attribute("listen").is_some() {
                push_attr_replacement(xml, graphics, "listen", &manifest.vnc_listen, replacements)?;
            }
            if let Some(listen) = optional_child(graphics, "listen")
                && listen.attribute("address").is_some()
            {
                push_attr_replacement(xml, listen, "address", &manifest.vnc_listen, replacements)?;
            }

            let port = manifest
                .vnc_port
                .map(|port| port.to_string())
                .unwrap_or_else(|| "-1".to_string());
            let autoport = if manifest.vnc_port.is_some() {
                "no"
            } else {
                "yes"
            };
            if graphics.attribute("port").is_some() {
                push_attr_replacement(xml, graphics, "port", &port, replacements)?;
            }
            if graphics.attribute("autoport").is_some() {
                push_attr_replacement(xml, graphics, "autoport", autoport, replacements)?;
            }
        }
    }

    Ok(())
}

fn patch_serial_log(
    xml: &str,
    devices: Node<'_, '_>,
    path: &Path,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let desired = path.display().to_string();
    let mut patched = false;

    for console in devices
        .children()
        .filter(|child| child.has_tag_name("console") && child.attribute("type") == Some("file"))
    {
        if let Some(source) = optional_child(console, "source")
            && source.attribute("path").is_some()
        {
            push_attr_replacement(xml, source, "path", &desired, replacements)?;
            patched = true;
        }
    }

    for serial in devices
        .children()
        .filter(|child| child.has_tag_name("serial") && child.attribute("type") == Some("file"))
    {
        if let Some(source) = optional_child(serial, "source")
            && source.attribute("path").is_some()
        {
            push_attr_replacement(xml, source, "path", &desired, replacements)?;
            patched = true;
        }
    }

    if !patched {
        bail!("cannot update existing domain XML because file console/serial log is missing");
    }

    Ok(())
}

fn find_disk<'a, 'input>(
    devices: Node<'a, 'input>,
    device: &str,
    target_dev: Option<&str>,
) -> Result<Option<Node<'a, 'input>>> {
    for disk in devices
        .children()
        .filter(|child| child.has_tag_name("disk") && child.attribute("device") == Some(device))
    {
        if let Some(target_dev) = target_dev
            && disk_target_dev(disk).as_deref() != Some(target_dev)
        {
            continue;
        }

        return Ok(Some(disk));
    }

    Ok(None)
}

fn push_text_replacement(
    xml: &str,
    node: Node<'_, '_>,
    value: &str,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let range = node_text_range(node)?;
    if &xml[range.clone()] != value {
        replacements.push(XmlReplacement {
            range,
            value: value.to_string(),
        });
    }

    Ok(())
}

fn push_attr_replacement(
    xml: &str,
    node: Node<'_, '_>,
    attr_name: &str,
    value: &str,
    replacements: &mut Vec<XmlReplacement>,
) -> Result<()> {
    let attr = node
        .attributes()
        .find(|attr| attr.name() == attr_name)
        .with_context(|| {
            format!(
                "domain XML <{}> is missing {attr_name} attribute",
                node.tag_name().name()
            )
        })?;
    let range = attr.range_value();
    let escaped = escape_xml_value(value);
    if &xml[range.clone()] != escaped {
        replacements.push(XmlReplacement {
            range,
            value: escaped,
        });
    }

    Ok(())
}

fn node_text_range(node: Node<'_, '_>) -> Result<Range<usize>> {
    node.children()
        .find(|child| child.is_text())
        .map(|child| child.range())
        .with_context(|| format!("domain XML <{}> is missing text", node.tag_name().name()))
}

fn apply_xml_replacements(xml: &str, mut replacements: Vec<XmlReplacement>) -> String {
    replacements.sort_by_key(|replacement| replacement.range.start);

    let mut output = xml.to_string();
    for replacement in replacements.into_iter().rev() {
        output.replace_range(replacement.range, &replacement.value);
    }

    output
}

fn line_start(xml: &str, pos: usize) -> usize {
    xml[..pos].rfind('\n').map(|index| index + 1).unwrap_or(0)
}

fn line_end(xml: &str, pos: usize) -> usize {
    xml[pos..]
        .find('\n')
        .map(|index| pos + index + 1)
        .unwrap_or(xml.len())
}

fn boot_device_name(device: BootDevice) -> &'static str {
    match device {
        BootDevice::Hd => "hd",
        BootDevice::Cdrom => "cdrom",
    }
}

fn escape_xml_value(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn required_child<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Result<Node<'a, 'input>> {
    node.children()
        .find(|child| child.has_tag_name(tag))
        .with_context(|| format!("domain XML is missing <{tag}>"))
}

fn optional_child<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|child| child.has_tag_name(tag))
}

fn required_child_text<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Result<&'a str> {
    required_child(node, tag)?
        .text()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("domain XML <{tag}> is empty"))
}

fn memory_mib(domain: Node<'_, '_>) -> Result<u64> {
    let memory = required_child(domain, "memory")?;
    let value = memory
        .text()
        .map(str::trim)
        .context("domain XML <memory> is empty")?
        .parse::<u64>()
        .context("failed to parse domain memory")?;

    match memory.attribute("unit").unwrap_or("KiB") {
        "KiB" => {
            if value % 1024 != 0 {
                bail!("domain memory {value} KiB cannot be represented as whole MiB");
            }
            Ok(value / 1024)
        }
        "MiB" => Ok(value),
        "GiB" => value
            .checked_mul(1024)
            .context("domain memory is too large"),
        unit => bail!("unsupported domain memory unit {unit:?}"),
    }
}

fn boot_order(domain: Node<'_, '_>) -> Result<Vec<String>> {
    let os = required_child(domain, "os")?;
    let boot = os
        .children()
        .filter(|child| child.has_tag_name("boot"))
        .map(|boot| {
            let dev = boot
                .attribute("dev")
                .context("domain XML <boot> is missing dev attribute")?;
            match dev {
                "hd" | "cdrom" => Ok(dev.to_string()),
                _ => bail!("unsupported boot device {dev:?}"),
            }
        })
        .collect::<Result<Vec<_>>>()?;

    if boot.is_empty() {
        bail!("domain XML is missing boot order");
    }

    Ok(boot)
}

fn disk_source_path(
    devices: Node<'_, '_>,
    device: &str,
    target_dev: Option<&str>,
) -> Result<PathBuf> {
    optional_disk_source_path(devices, device, target_dev)?.with_context(|| {
        let target = target_dev
            .map(|target| format!(" target {target}"))
            .unwrap_or_default();
        format!("domain XML is missing {device} disk{target}")
    })
}

fn optional_disk_source_path(
    devices: Node<'_, '_>,
    device: &str,
    target_dev: Option<&str>,
) -> Result<Option<PathBuf>> {
    for disk in devices
        .children()
        .filter(|child| child.has_tag_name("disk") && child.attribute("device") == Some(device))
    {
        if let Some(target_dev) = target_dev
            && disk_target_dev(disk).as_deref() != Some(target_dev)
        {
            continue;
        }

        let source = optional_child(disk, "source")
            .and_then(|source| source.attribute("file"))
            .with_context(|| format!("domain XML {device} disk is missing source file"))?;
        return Ok(Some(PathBuf::from(source)));
    }

    Ok(None)
}

fn disk_target_dev(disk: Node<'_, '_>) -> Option<String> {
    optional_child(disk, "target")
        .and_then(|target| target.attribute("dev"))
        .map(str::to_string)
}

fn network_name(devices: Node<'_, '_>) -> Result<String> {
    let interface = devices
        .children()
        .find(|child| child.has_tag_name("interface") && child.attribute("type") == Some("network"))
        .context("domain XML is missing network interface")?;
    Ok(optional_child(interface, "source")
        .and_then(|source| source.attribute("network"))
        .context("domain XML network interface is missing source network")?
        .to_string())
}

fn graphics_config(devices: Node<'_, '_>) -> Result<(GraphicsMode, String, Option<u16>)> {
    let Some(graphics) = devices
        .children()
        .find(|child| child.has_tag_name("graphics"))
    else {
        return Ok((GraphicsMode::None, default_vm_vnc_listen(), None));
    };

    match graphics.attribute("type") {
        Some("vnc") => {
            let listen = graphics
                .attribute("listen")
                .map(str::to_string)
                .or_else(|| {
                    optional_child(graphics, "listen")
                        .and_then(|listen| listen.attribute("address"))
                        .map(str::to_string)
                })
                .unwrap_or_else(default_vm_vnc_listen);
            let vnc_port = match graphics.attribute("port") {
                Some("-1") | None => None,
                Some(port) => Some(port.parse().context("failed to parse VNC port")?),
            };
            Ok((GraphicsMode::Vnc, listen, vnc_port))
        }
        Some(kind) => bail!("unsupported graphics type {kind:?}"),
        None => bail!("domain XML graphics device is missing type"),
    }
}

fn serial_log_path(devices: Node<'_, '_>) -> Option<PathBuf> {
    devices
        .children()
        .find(|child| child.has_tag_name("console") && child.attribute("type") == Some("file"))
        .and_then(|console| optional_child(console, "source"))
        .and_then(|source| source.attribute("path"))
        .map(PathBuf::from)
}

fn print_apply_diff(
    current_xml: &str,
    name: &str,
    manifest_path: &Path,
    desired_xml: &str,
    color: bool,
) {
    if current_xml == desired_xml {
        println!("[qtr] no changes");
        return;
    }

    let current_header = if current_xml.is_empty() {
        "/dev/null".to_string()
    } else {
        format!("current/libvirt/{name}")
    };
    let desired_path = manifest_path.strip_prefix("/").unwrap_or(manifest_path);
    let desired_header = format!("desired/{}", desired_path.display());
    let diff = TextDiff::from_lines(current_xml, desired_xml);
    let diff = diff
        .unified_diff()
        .context_radius(3)
        .header(&current_header, &desired_header)
        .to_string();

    if color {
        print!("{}", colorize_unified_diff(&diff));
    } else {
        print!("{diff}");
    }
}

fn should_color(mode: ColorMode) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Auto => io::stdout().is_terminal(),
        ColorMode::Never => false,
    }
}

fn colorize_unified_diff(diff: &str) -> String {
    diff.split_inclusive('\n')
        .map(colorize_diff_line)
        .collect::<String>()
}

fn colorize_diff_line(line: &str) -> String {
    let (content, newline) = line
        .strip_suffix('\n')
        .map_or((line, ""), |line| (line, "\n"));
    let color = if content.starts_with("--- ") || content.starts_with('-') {
        Some("\x1b[31m")
    } else if content.starts_with("+++ ") || content.starts_with('+') {
        Some("\x1b[32m")
    } else if content.starts_with("@@") {
        Some("\x1b[36m")
    } else {
        None
    };

    match color {
        Some(color) => format!("{color}{content}\x1b[0m{newline}"),
        None => line.to_string(),
    }
}

fn current_domain_xml(connect_uri: &str, name: &str) -> Result<String> {
    let conn = connect_read_only(connect_uri)?;
    let domain = match Domain::lookup_by_name(&conn, name) {
        Ok(domain) => domain,
        Err(_) => return Ok(String::new()),
    };

    domain
        .get_xml_desc(sys::VIR_DOMAIN_XML_INACTIVE)
        .with_context(|| format!("failed to query inactive domain XML for {name}"))
}

fn existing_domain_xml(connect_uri: &str, name: &str) -> Result<String> {
    let conn = connect_read_only(connect_uri)?;
    let domain = lookup_domain(&conn, name)?;
    domain
        .get_xml_desc(sys::VIR_DOMAIN_XML_INACTIVE)
        .with_context(|| format!("failed to query inactive domain XML for {name}"))
}

fn normalize_manifest_paths(manifest: &mut VmManifest, base_dir: &Path) -> Result<()> {
    manifest.system_disk = manifest_relative_path(base_dir, &manifest.system_disk);

    if let Some(cdrom) = &manifest.cdrom {
        manifest.cdrom = Some(manifest_relative_path(base_dir, cdrom));
    }

    let serial_log = manifest
        .serial_log
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!(".tmp/logs/{}.serial.log", manifest.name)));
    manifest.serial_log = Some(manifest_relative_path(base_dir, &serial_log));

    Ok(())
}

fn manifest_relative_path(base_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn validate_manifest(manifest: &VmManifest) -> Result<()> {
    if !manifest.system_disk.exists() {
        bail!(
            "system disk {} does not exist",
            manifest.system_disk.display()
        );
    }

    if let Some(cdrom) = &manifest.cdrom
        && !cdrom.exists()
    {
        bail!("cdrom ISO {} does not exist", cdrom.display());
    }

    Ok(())
}

fn manifest_boot_order(manifest: &VmManifest) -> String {
    match &manifest.boot {
        Some(boot) => boot.join(","),
        None if manifest.cdrom.is_some() => "cdrom,hd".to_string(),
        None => "hd".to_string(),
    }
}

fn list(args: VmListArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let flags = sys::VIR_CONNECT_LIST_DOMAINS_ACTIVE | sys::VIR_CONNECT_LIST_DOMAINS_INACTIVE;
    let mut rows = conn
        .list_all_domains(flags)
        .context("failed to list domains")?
        .into_iter()
        .map(|domain| domain_list_row(&domain))
        .collect::<Result<Vec<_>>>()?;

    rows.sort_by(|left, right| left.name.cmp(&right.name));

    println!("{:<32} {:<12} ID", "NAME", "STATE");
    for row in rows {
        println!("{:<32} {:<12} {}", row.name, row.state, row.id);
    }

    Ok(())
}

struct DomainListRow {
    name: String,
    state: &'static str,
    id: String,
}

fn domain_list_row(domain: &Domain) -> Result<DomainListRow> {
    let name = domain.get_name().context("failed to query domain name")?;
    let (state, _) = domain
        .get_state()
        .with_context(|| format!("failed to query domain {name} state"))?;
    let id = domain
        .get_id()
        .map(|id| id.to_string())
        .unwrap_or_else(|| "-".to_string());

    Ok(DomainListRow {
        name,
        state: domain_state_name(state),
        id,
    })
}

fn domain_state_name(state: sys::virDomainState) -> &'static str {
    match state {
        sys::VIR_DOMAIN_NOSTATE => "nostate",
        sys::VIR_DOMAIN_RUNNING => "running",
        sys::VIR_DOMAIN_BLOCKED => "blocked",
        sys::VIR_DOMAIN_PAUSED => "paused",
        sys::VIR_DOMAIN_SHUTDOWN => "shutdown",
        sys::VIR_DOMAIN_SHUTOFF => "shutoff",
        sys::VIR_DOMAIN_CRASHED => "crashed",
        sys::VIR_DOMAIN_PMSUSPENDED => "pmsuspended",
        _ => "unknown",
    }
}

fn create(mut args: VmCreateArgs) -> Result<Domain> {
    normalize_create_args(&mut args)?;
    let serial_log = args.serial_log.clone();
    let domain = create_normalized(args)?;
    if let Some(serial_log) = serial_log {
        eprintln!("[qtr] serial log: {}", serial_log.display());
    }

    Ok(domain)
}

fn create_normalized(args: VmCreateArgs) -> Result<Domain> {
    prepare_system_disk(&args)?;

    let boot = default_boot_order(&args);
    let boot_devices = domain_xml::parse_boot_devices(&boot)?;
    if boot_devices.contains(&BootDevice::Cdrom) && args.cdrom.is_none() {
        bail!("boot order contains cdrom but --cdrom was not provided");
    }

    let xml = build_vm_launch_domain_xml(VmLaunchDomainSpec {
        name: &args.name,
        memory_mib: args.memory_mib,
        vcpus: args.vcpus,
        system_disk: &args.system_disk,
        cdrom: args.cdrom.as_deref(),
        serial_log: args.serial_log.as_deref(),
        boot_devices: &boot_devices,
        network: &args.network,
        graphics: GraphicsSpec {
            mode: args.graphics,
            vnc_listen: &args.vnc_listen,
            vnc_port: args.vnc_port,
        },
    });

    let conn = connect(&args.connect_uri)?;
    let domain = Domain::define_xml(&conn, &xml)
        .with_context(|| format!("failed to define domain {}", args.name))?;

    eprintln!("[qtr] defined VM: {}", args.name);
    Ok(domain)
}

fn launch(mut args: VmLaunchArgs) -> Result<()> {
    normalize_create_args(&mut args.create)?;

    let name = args.create.name.clone();
    let graphics = args.create.graphics;
    let vnc_listen = args.create.vnc_listen.clone();
    let system_disk = args.create.system_disk.clone();
    let wait = args.wait_shutdown;
    let domain = create_normalized(args.create)?;

    start_domain(&domain, &name)?;

    if graphics == GraphicsMode::Vnc {
        print_vnc_endpoint(&domain, &vnc_listen)?;
    }
    print_serial_log(&domain)?;

    if wait {
        eprintln!("[qtr] waiting for guest shutdown...");
        wait_shutdown_domain(&domain, &name)?;
        domain
            .undefine()
            .with_context(|| format!("failed to undefine domain {name}"))?;
        eprintln!("[qtr] undefined VM: {name}");
        eprintln!("[qtr] system disk saved: {}", system_disk.display());
    }

    Ok(())
}

fn start(args: VmNameArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    start_domain(&domain, &args.name)?;

    print_vnc_endpoint(&domain, "127.0.0.1")?;
    print_serial_log(&domain)?;

    Ok(())
}

fn vnc(args: VmNameArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {} state", args.name))?
    {
        bail!("domain {} is not active", args.name);
    }

    let endpoint = query_vnc_endpoint(&domain, "127.0.0.1")?.with_context(|| {
        format!(
            "domain {} does not expose an active VNC endpoint",
            args.name
        )
    })?;
    println!("{endpoint}");

    Ok(())
}

fn exec(args: VmExecArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {} state", args.name))?
    {
        bail!("domain {} is not active", args.name);
    }

    let timeout = Duration::from_secs(args.timeout_secs);
    guest_agent::wait_ready(&domain, timeout)
        .with_context(|| format!("guest agent is not ready for domain {}", args.name))?;

    let command = args.command.join(" ");
    let result = guest_agent::run_command(&domain, &command, timeout)
        .with_context(|| format!("failed to run guest command in domain {}", args.name))?;

    io::stdout()
        .write_all(&result.stdout)
        .context("failed to write guest stdout")?;
    io::stderr()
        .write_all(&result.stderr)
        .context("failed to write guest stderr")?;

    if result.exitcode != 0 {
        bail!("guest command exited with {}", result.exitcode);
    }

    Ok(())
}

fn wait_shutdown_command(args: VmNameArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    eprintln!("[qtr] waiting for guest shutdown...");
    wait_shutdown_domain(&domain, &args.name)
}

fn shutdown(args: VmShutdownArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {} state", args.name))?
    {
        eprintln!("[qtr] VM already stopped: {}", args.name);
        return Ok(());
    }

    domain
        .shutdown()
        .with_context(|| format!("failed to request shutdown for domain {}", args.name))?;
    eprintln!("[qtr] shutdown requested: {}", args.name);

    if args.wait {
        wait_shutdown_domain(&domain, &args.name)?;
    }

    Ok(())
}

fn destroy(args: VmNameArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    if !domain
        .is_active()
        .with_context(|| format!("failed to query domain {} state", args.name))?
    {
        eprintln!("[qtr] VM already stopped: {}", args.name);
        return Ok(());
    }

    domain
        .destroy()
        .with_context(|| format!("failed to destroy domain {}", args.name))?;
    eprintln!("[qtr] destroyed VM: {}", args.name);

    Ok(())
}

fn undefine(args: VmNameArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let domain = lookup_domain(&conn, &args.name)?;
    if domain
        .is_active()
        .with_context(|| format!("failed to query domain {} state", args.name))?
    {
        bail!(
            "domain {} is active; shutdown or destroy it first",
            args.name
        );
    }

    domain
        .undefine()
        .with_context(|| format!("failed to undefine domain {}", args.name))?;
    eprintln!("[qtr] undefined VM: {}", args.name);

    Ok(())
}

fn connect(uri: &str) -> Result<Connect> {
    Connect::open(Some(uri)).with_context(|| format!("failed to connect to libvirt at {uri}"))
}

fn connect_read_only(uri: &str) -> Result<Connect> {
    Connect::open_read_only(Some(uri))
        .with_context(|| format!("failed to connect to libvirt read-only at {uri}"))
}

fn lookup_domain(conn: &Connect, name: &str) -> Result<Domain> {
    Domain::lookup_by_name(conn, name).with_context(|| format!("failed to find domain {name}"))
}

fn start_domain(domain: &Domain, name: &str) -> Result<()> {
    if domain
        .is_active()
        .with_context(|| format!("failed to query domain {name} state"))?
    {
        eprintln!("[qtr] VM already running: {name}");
        return Ok(());
    }

    domain
        .create()
        .with_context(|| format!("failed to start domain {name}"))?;
    eprintln!("[qtr] started VM: {name}");

    Ok(())
}

fn prepare_system_disk(args: &VmCreateArgs) -> Result<()> {
    match &args.create_system_disk {
        Some(size) => disk::create_image(&args.system_disk, DiskFormat::Qcow2, size),
        None => {
            if !args.system_disk.exists() {
                bail!(
                    "system disk {} does not exist; pass --create-system-disk to create it",
                    args.system_disk.display()
                );
            }
            Ok(())
        }
    }
}

fn prepare_serial_log(args: &VmCreateArgs) -> Result<()> {
    prepare_serial_log_path(args.serial_log.as_deref())
}

fn prepare_serial_log_path(path: Option<&Path>) -> Result<()> {
    let Some(path) = path else { return Ok(()) };
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    Ok(())
}

fn normalize_create_args(args: &mut VmCreateArgs) -> Result<()> {
    args.system_disk = absolute_path(&args.system_disk)?;

    let serial_log = args
        .serial_log
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!(".tmp/logs/{}.serial.log", args.name)));
    args.serial_log = Some(absolute_path(&serial_log)?);

    if let Some(cdrom) = &args.cdrom {
        let cdrom = absolute_path(cdrom)?;
        if !cdrom.exists() {
            bail!("cdrom ISO {} does not exist", cdrom.display());
        }
        args.cdrom = Some(cdrom);
    }

    prepare_serial_log(args)?;

    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    Ok(env::current_dir()
        .context("failed to determine current directory")?
        .join(path))
}

fn default_boot_order(args: &VmCreateArgs) -> String {
    match &args.boot {
        Some(boot) => boot.clone(),
        None if args.cdrom.is_some() => "cdrom,hd".to_string(),
        None => "hd".to_string(),
    }
}

fn print_vnc_endpoint(domain: &Domain, fallback_listen: &str) -> Result<()> {
    match query_vnc_endpoint_spec(domain, fallback_listen)? {
        Some(endpoint) => {
            eprintln!("[qtr] VNC: {}", endpoint.display());
            if endpoint.is_wildcard() {
                let endpoints = local_vnc_endpoints(&endpoint.port);
                if !endpoints.is_empty() {
                    eprintln!("[qtr] VNC endpoints:");
                    for endpoint in endpoints {
                        eprintln!("[qtr]   {endpoint}");
                    }
                }
            }
        }
        None => eprintln!("[qtr] VNC: enabled, but port was not found in domain XML"),
    }

    Ok(())
}

fn query_vnc_endpoint(domain: &Domain, fallback_listen: &str) -> Result<Option<String>> {
    Ok(query_vnc_endpoint_spec(domain, fallback_listen)?.map(|endpoint| endpoint.display()))
}

fn query_vnc_endpoint_spec(domain: &Domain, fallback_listen: &str) -> Result<Option<VncEndpoint>> {
    let xml = domain
        .get_xml_desc(0)
        .context("failed to query domain XML")?;
    Ok(parse_vnc_endpoint(&xml, fallback_listen))
}

fn print_serial_log(domain: &Domain) -> Result<()> {
    if let Some(path) = query_serial_log(domain)? {
        eprintln!("[qtr] serial log: {path}");
    }

    Ok(())
}

fn query_serial_log(domain: &Domain) -> Result<Option<String>> {
    let xml = domain
        .get_xml_desc(0)
        .context("failed to query domain XML")?;
    Ok(parse_serial_log(&xml))
}

fn parse_serial_log(xml: &str) -> Option<String> {
    let console_start = xml.find("<console type='file'")?;
    let console_xml = &xml[console_start..];
    let console_end = console_xml.find("</console>")?;
    let console_xml = &console_xml[..console_end];
    let source_start = console_xml.find("<source ")?;
    let source_xml = &console_xml[source_start..];
    let source_end = source_xml.find('>')?;
    parse_attr(&source_xml[..source_end], "path")
}

#[derive(Debug)]
struct VncEndpoint {
    listen: String,
    port: String,
}

impl VncEndpoint {
    fn display(&self) -> String {
        format_endpoint(&self.listen, &self.port)
    }

    fn is_wildcard(&self) -> bool {
        matches!(self.listen.as_str(), "0.0.0.0" | "::")
    }
}

fn parse_vnc_endpoint(xml: &str, fallback_listen: &str) -> Option<VncEndpoint> {
    let graphics_start = xml.find("<graphics type='vnc'")?;
    let graphics = &xml[graphics_start..];
    let graphics_end = graphics.find('>')?;
    let graphics_tag = &graphics[..graphics_end];
    let port = parse_attr(graphics_tag, "port")?;
    if port == "-1" {
        return None;
    }

    let listen = parse_attr(graphics_tag, "listen")
        .or_else(|| parse_nested_listen(graphics))
        .unwrap_or_else(|| fallback_listen.to_string());

    Some(VncEndpoint { listen, port })
}

fn local_vnc_endpoints(port: &str) -> Vec<String> {
    let output = match Command::new("ip").args(["-o", "addr", "show"]).output() {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    let stdout = match String::from_utf8(output.stdout) {
        Ok(stdout) => stdout,
        Err(_) => return Vec::new(),
    };

    let ips = stdout
        .lines()
        .flat_map(local_ips_from_ip_addr_line)
        .collect();
    format_vnc_endpoints(ips, port)
}

fn local_ips_from_ip_addr_line(line: &str) -> Vec<IpAddr> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    fields
        .windows(2)
        .filter_map(|window| match window {
            ["inet" | "inet6", value] => value.split_once('/').map(|(addr, _)| addr),
            _ => None,
        })
        .filter_map(|addr| addr.parse::<IpAddr>().ok())
        .filter(|addr| !addr.is_unspecified())
        .collect()
}

fn format_vnc_endpoints(ips: BTreeSet<IpAddr>, port: &str) -> Vec<String> {
    ips.into_iter()
        .map(|ip| format_endpoint(&ip.to_string(), port))
        .collect()
}

fn format_endpoint(host: &str, port: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn parse_nested_listen(graphics_xml: &str) -> Option<String> {
    let listen_start = graphics_xml.find("<listen ")?;
    let listen = &graphics_xml[listen_start..];
    let listen_end = listen.find('>')?;
    parse_attr(&listen[..listen_end], "address")
}

fn parse_attr(tag: &str, name: &str) -> Option<String> {
    let single = format!("{name}='");
    if let Some(start) = tag.find(&single) {
        let value = &tag[start + single.len()..];
        let end = value.find('\'')?;
        return Some(value[..end].to_string());
    }

    let double = format!("{name}=\"");
    if let Some(start) = tag.find(&double) {
        let value = &tag[start + double.len()..];
        let end = value.find('"')?;
        return Some(value[..end].to_string());
    }

    None
}

fn wait_shutdown_domain(domain: &Domain, name: &str) -> Result<()> {
    loop {
        if !domain
            .is_active()
            .with_context(|| format!("failed to query domain {name} state"))?
        {
            return Ok(());
        }

        thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dumps_qtr_domain_xml_to_manifest() {
        let boot_devices = [BootDevice::Cdrom, BootDevice::Hd];
        let xml = build_vm_launch_domain_xml(VmLaunchDomainSpec {
            name: "install-os",
            memory_mib: 4096,
            vcpus: 2,
            system_disk: Path::new("/var/lib/libvirt/images/sys.qcow2"),
            cdrom: Some(Path::new("/isos/os.iso")),
            serial_log: Some(Path::new("/logs/install-os.serial.log")),
            boot_devices: &boot_devices,
            network: "default",
            graphics: GraphicsSpec {
                mode: GraphicsMode::Vnc,
                vnc_listen: "0.0.0.0",
                vnc_port: Some(5901),
            },
        });

        let manifest = manifest_from_domain_xml(&xml).expect("domain XML should parse");

        assert_eq!(manifest.name, "install-os");
        assert_eq!(
            manifest.system_disk,
            PathBuf::from("/var/lib/libvirt/images/sys.qcow2")
        );
        assert_eq!(manifest.cdrom, Some(PathBuf::from("/isos/os.iso")));
        assert_eq!(
            manifest.boot,
            Some(vec!["cdrom".to_string(), "hd".to_string()])
        );
        assert_eq!(manifest.memory_gib, 4);
        assert_eq!(manifest.vcpus, 2);
        assert_eq!(manifest.network, "default");
        assert_eq!(manifest.graphics, GraphicsMode::Vnc);
        assert_eq!(manifest.vnc_listen, "0.0.0.0");
        assert_eq!(manifest.vnc_port, Some(5901));
        assert_eq!(
            manifest.serial_log,
            Some(PathBuf::from("/logs/install-os.serial.log"))
        );
    }

    #[test]
    fn patches_existing_domain_xml_without_rebuilding_it() {
        let xml = r#"<domain type='kvm'>
  <name>install-os</name>
  <uuid>c194be5c-a0ba-4e90-8b23-18c8df0825f1</uuid>
  <memory unit='KiB'>4194304</memory>
  <currentMemory unit='KiB'>4194304</currentMemory>
  <vcpu placement='static'>2</vcpu>
  <os>
    <type arch='x86_64' machine='pc-i440fx-10.2'>hvm</type>
    <boot dev='cdrom'/>
    <boot dev='hd'/>
  </os>
  <features>
    <acpi/>
    <apic/>
  </features>
  <cpu mode='host-passthrough' check='none' migratable='off'/>
  <devices>
    <emulator>/usr/bin/qemu-system-x86_64</emulator>
    <disk type='file' device='disk'>
      <driver name='qemu' type='qcow2'/>
      <source file='/home/fanmi/workspace/qtr/.tmp/disks/sys.qcow2'/>
      <target dev='vda' bus='virtio'/>
      <address type='pci' domain='0x0000' bus='0x00' slot='0x07' function='0x0'/>
    </disk>
    <disk type='file' device='cdrom'>
      <driver name='qemu' type='raw'/>
      <source file='/home/fanmi/workspace/qtr/.tmp/iso/CentOS-7-x86_64-DVD-2207-02.iso'/>
      <target dev='sda' bus='sata'/>
      <readonly/>
      <address type='drive' controller='0' bus='0' target='0' unit='0'/>
    </disk>
    <controller type='usb' index='0' model='qemu-xhci'>
      <address type='pci' domain='0x0000' bus='0x00' slot='0x04' function='0x0'/>
    </controller>
    <interface type='network'>
      <mac address='52:54:00:1c:92:5f'/>
      <source network='default'/>
      <model type='virtio'/>
      <address type='pci' domain='0x0000' bus='0x00' slot='0x03' function='0x0'/>
    </interface>
    <serial type='file'>
      <source path='/home/fanmi/workspace/qtr/.tmp/logs/install-os.serial.log'/>
      <target type='isa-serial' port='0'>
        <model name='isa-serial'/>
      </target>
    </serial>
    <console type='file'>
      <source path='/home/fanmi/workspace/qtr/.tmp/logs/install-os.serial.log'/>
      <target type='serial' port='0'/>
    </console>
    <channel type='unix'>
      <target type='virtio' name='org.qemu.guest_agent.0'/>
      <address type='virtio-serial' controller='0' bus='0' port='1'/>
    </channel>
    <input type='tablet' bus='usb'>
      <address type='usb' bus='0' port='1'/>
    </input>
    <graphics type='vnc' port='-1' autoport='yes' listen='0.0.0.0'>
      <listen type='address' address='0.0.0.0'/>
    </graphics>
    <video>
      <model type='cirrus' vram='16384' heads='1' primary='yes'/>
      <address type='pci' domain='0x0000' bus='0x00' slot='0x02' function='0x0'/>
    </video>
  </devices>
</domain>
"#;
        let manifest = VmManifest {
            name: "install-os".to_string(),
            system_disk: PathBuf::from("/home/fanmi/workspace/qtr/.tmp/disks/sys.qcow2"),
            cdrom: Some(PathBuf::from(
                "/home/fanmi/workspace/qtr/.tmp/iso/CentOS-7-x86_64-DVD-2207-02.iso",
            )),
            boot: Some(vec!["hd".to_string()]),
            memory_gib: 4,
            vcpus: 2,
            network: "default".to_string(),
            graphics: GraphicsMode::Vnc,
            vnc_listen: "0.0.0.0".to_string(),
            vnc_port: None,
            serial_log: Some(PathBuf::from(
                "/home/fanmi/workspace/qtr/.tmp/logs/install-os.serial.log",
            )),
        };
        let boot_devices = [BootDevice::Hd];

        let patched =
            patch_domain_xml(xml, &manifest, &boot_devices, 4096).expect("XML should patch");

        assert!(patched.contains("<uuid>c194be5c-a0ba-4e90-8b23-18c8df0825f1</uuid>"));
        assert!(patched.contains("machine='pc-i440fx-10.2'"));
        assert!(patched.contains("<memory unit='KiB'>4194304</memory>"));
        assert!(patched.contains(
            "<address type='pci' domain='0x0000' bus='0x00' slot='0x07' function='0x0'/>"
        ));
        assert!(patched.contains("<video>"));
        assert!(!patched.contains("<boot dev='cdrom'/>"));
        assert!(patched.contains("    <boot dev='hd'/>\n"));
    }

    #[test]
    fn colorizes_unified_diff_lines() {
        let diff = "--- old\n+++ new\n@@ -1 +1 @@\n-old\n+new\n same\n";

        let colored = colorize_unified_diff(diff);

        assert!(colored.contains("\x1b[31m--- old\x1b[0m\n"));
        assert!(colored.contains("\x1b[32m+++ new\x1b[0m\n"));
        assert!(colored.contains("\x1b[36m@@ -1 +1 @@\x1b[0m\n"));
        assert!(colored.contains("\x1b[31m-old\x1b[0m\n"));
        assert!(colored.contains("\x1b[32m+new\x1b[0m\n"));
        assert!(colored.contains(" same\n"));
    }
}
