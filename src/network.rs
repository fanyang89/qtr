use std::net::Ipv4Addr;

use anyhow::{Context, Result, bail};
use virt::{connect::Connect, error::clear_error_callback, network::Network};

use crate::config::{NetArgs, NetCommand, NetCreateArgs, NetNameArgs};

pub fn run(args: NetArgs) -> Result<()> {
    clear_error_callback();

    match args.command {
        NetCommand::Create(args) => create(args),
        NetCommand::Start(args) => start(args),
        NetCommand::Stop(args) => stop(args),
        NetCommand::Undefine(args) => undefine(args),
        NetCommand::Info(args) => info(args),
    }
}

fn create(args: NetCreateArgs) -> Result<()> {
    validate_ipv4("address", &args.address)?;
    validate_ipv4("netmask", &args.netmask)?;
    validate_ipv4("dhcp-start", &args.dhcp_start)?;
    validate_ipv4("dhcp-end", &args.dhcp_end)?;

    let xml = build_nat_network_xml(&args);
    let conn = connect(&args.connect_uri)?;
    let network = Network::define_xml(&conn, &xml)
        .with_context(|| format!("failed to define network {}", args.name))?;

    eprintln!("[qtr] defined network: {}", args.name);

    if args.autostart {
        network
            .set_autostart(true)
            .with_context(|| format!("failed to enable autostart for network {}", args.name))?;
        eprintln!("[qtr] enabled network autostart: {}", args.name);
    }

    if args.start {
        start_network(&network, &args.name)?;
    }

    Ok(())
}

fn start(args: NetNameArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let network = lookup_network(&conn, &args.name)?;
    start_network(&network, &args.name)
}

fn stop(args: NetNameArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let network = lookup_network(&conn, &args.name)?;
    if !network
        .is_active()
        .with_context(|| format!("failed to query network {} state", args.name))?
    {
        eprintln!("[qtr] network already stopped: {}", args.name);
        return Ok(());
    }

    network
        .destroy()
        .with_context(|| format!("failed to stop network {}", args.name))?;
    eprintln!("[qtr] stopped network: {}", args.name);

    Ok(())
}

fn undefine(args: NetNameArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let network = lookup_network(&conn, &args.name)?;
    if network
        .is_active()
        .with_context(|| format!("failed to query network {} state", args.name))?
    {
        bail!("network {} is active; stop it first", args.name);
    }

    network
        .undefine()
        .with_context(|| format!("failed to undefine network {}", args.name))?;
    eprintln!("[qtr] undefined network: {}", args.name);

    Ok(())
}

fn info(args: NetNameArgs) -> Result<()> {
    let conn = connect(&args.connect_uri)?;
    let network = lookup_network(&conn, &args.name)?;
    let active = network
        .is_active()
        .with_context(|| format!("failed to query network {} state", args.name))?;
    let autostart = network
        .get_autostart()
        .with_context(|| format!("failed to query network {} autostart", args.name))?;
    let bridge = network
        .get_bridge_name()
        .unwrap_or_else(|_| "<none>".to_string());

    println!("name: {}", args.name);
    println!("active: {active}");
    println!("autostart: {autostart}");
    println!("bridge: {bridge}");

    Ok(())
}

fn connect(uri: &str) -> Result<Connect> {
    Connect::open(Some(uri)).with_context(|| format!("failed to connect to libvirt at {uri}"))
}

fn lookup_network(conn: &Connect, name: &str) -> Result<Network> {
    Network::lookup_by_name(conn, name).with_context(|| format!("failed to find network {name}"))
}

fn start_network(network: &Network, name: &str) -> Result<()> {
    if network
        .is_active()
        .with_context(|| format!("failed to query network {name} state"))?
    {
        eprintln!("[qtr] network already active: {name}");
        return Ok(());
    }

    network
        .create()
        .with_context(|| format!("failed to start network {name}"))?;
    eprintln!("[qtr] started network: {name}");

    Ok(())
}

fn build_nat_network_xml(args: &NetCreateArgs) -> String {
    let bridge_xml = args
        .bridge
        .as_deref()
        .map(|bridge| format!("  <bridge name='{}'/>\n", escape_xml(bridge)))
        .unwrap_or_default();

    format!(
        r#"<network>
  <name>{name}</name>
{bridge_xml}  <forward mode='nat'/>
  <ip address='{address}' netmask='{netmask}'>
    <dhcp>
      <range start='{dhcp_start}' end='{dhcp_end}'/>
    </dhcp>
  </ip>
</network>
"#,
        name = escape_xml(&args.name),
        bridge_xml = bridge_xml,
        address = escape_xml(&args.address),
        netmask = escape_xml(&args.netmask),
        dhcp_start = escape_xml(&args.dhcp_start),
        dhcp_end = escape_xml(&args.dhcp_end),
    )
}

fn validate_ipv4(name: &str, value: &str) -> Result<()> {
    value
        .parse::<Ipv4Addr>()
        .with_context(|| format!("invalid {name} IPv4 address: {value}"))?;
    Ok(())
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}
