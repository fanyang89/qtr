use std::{
    collections::BTreeSet,
    env,
    net::IpAddr,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use virt::{connect::Connect, domain::Domain, error::clear_error_callback, sys};

use crate::{
    config::{
        DiskFormat, GraphicsMode, VmArgs, VmCommand, VmCreateArgs, VmLaunchArgs, VmListArgs,
        VmNameArgs, VmShutdownArgs,
    },
    disk,
    domain_xml::{self, BootDevice, GraphicsSpec, VmLaunchDomainSpec, build_vm_launch_domain_xml},
};

pub fn run(args: VmArgs) -> Result<()> {
    clear_error_callback();

    match args.command {
        VmCommand::List(args) => list(args),
        VmCommand::Create(args) => create(args).map(|_| ()),
        VmCommand::Launch(args) => launch(args),
        VmCommand::Start(args) => start(args),
        VmCommand::Vnc(args) => vnc(args),
        VmCommand::WaitShutdown(args) => wait_shutdown_command(args),
        VmCommand::Shutdown(args) => shutdown(args),
        VmCommand::Destroy(args) => destroy(args),
        VmCommand::Undefine(args) => undefine(args),
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
    create_normalized(args)
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

fn normalize_create_args(args: &mut VmCreateArgs) -> Result<()> {
    args.system_disk = absolute_path(&args.system_disk)?;

    if let Some(cdrom) = &args.cdrom {
        let cdrom = absolute_path(cdrom)?;
        if !cdrom.exists() {
            bail!("cdrom ISO {} does not exist", cdrom.display());
        }
        args.cdrom = Some(cdrom);
    }

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
