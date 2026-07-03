use std::{fs, path::Path, process::Command, thread, time::Duration};

use anyhow::{Context, Result, bail};
use virt::{connect::Connect, domain::Domain, error::clear_error_callback};

use crate::{
    config::{GraphicsMode, VmArgs, VmCommand, VmLaunchArgs},
    domain_xml::{self, BootDevice, GraphicsSpec, VmLaunchDomainSpec, build_vm_launch_domain_xml},
};

pub fn run(args: VmArgs) -> Result<()> {
    match args.command {
        VmCommand::Launch(args) => launch(args),
    }
}

fn launch(args: VmLaunchArgs) -> Result<()> {
    clear_error_callback();

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
        boot_devices: &boot_devices,
        graphics: GraphicsSpec {
            mode: args.graphics,
            vnc_listen: &args.vnc_listen,
            vnc_port: args.vnc_port,
        },
    });

    let conn = Connect::open(Some(&args.connect_uri))
        .with_context(|| format!("failed to connect to libvirt at {}", args.connect_uri))?;
    let domain = Domain::define_xml(&conn, &xml)
        .with_context(|| format!("failed to define domain {}", args.name))?;

    domain
        .create()
        .with_context(|| format!("failed to start domain {}", args.name))?;

    eprintln!("[qtr] started VM: {}", args.name);
    if args.graphics == GraphicsMode::Vnc {
        print_vnc_endpoint(&domain, &args.vnc_listen)?;
    }

    if args.wait_shutdown {
        eprintln!("[qtr] waiting for guest shutdown...");
        wait_shutdown(&domain, &args.name)?;
        domain
            .undefine()
            .with_context(|| format!("failed to undefine domain {}", args.name))?;
        eprintln!("[qtr] undefined VM: {}", args.name);
        eprintln!("[qtr] system disk saved: {}", args.system_disk.display());
    }

    Ok(())
}

fn prepare_system_disk(args: &VmLaunchArgs) -> Result<()> {
    match &args.create_system_disk {
        Some(size) => create_system_disk(&args.system_disk, size),
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

fn create_system_disk(output: &Path, size: &str) -> Result<()> {
    if output.exists() {
        bail!("system disk {} already exists", output.display());
    }

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let status = Command::new("qemu-img")
        .arg("create")
        .arg("-f")
        .arg("qcow2")
        .arg(output)
        .arg(size)
        .status()
        .with_context(|| format!("failed to run qemu-img for {}", output.display()))?;

    if !status.success() {
        bail!("qemu-img failed to create system disk {}", output.display());
    }

    Ok(())
}

fn default_boot_order(args: &VmLaunchArgs) -> String {
    match &args.boot {
        Some(boot) => boot.clone(),
        None if args.cdrom.is_some() => "cdrom,hd".to_string(),
        None => "hd".to_string(),
    }
}

fn print_vnc_endpoint(domain: &Domain, fallback_listen: &str) -> Result<()> {
    let xml = domain
        .get_xml_desc(0)
        .context("failed to query started domain XML")?;

    match parse_vnc_endpoint(&xml, fallback_listen) {
        Some(endpoint) => eprintln!("[qtr] VNC: {endpoint}"),
        None => eprintln!("[qtr] VNC: enabled, but port was not found in domain XML"),
    }

    Ok(())
}

fn parse_vnc_endpoint(xml: &str, fallback_listen: &str) -> Option<String> {
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

    Some(format!("{listen}:{port}"))
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

fn wait_shutdown(domain: &Domain, name: &str) -> Result<()> {
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
