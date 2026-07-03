use std::{
    fs,
    io::{self, Write},
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use uuid::Uuid;
use virt::{connect::Connect, domain::Domain, error::clear_error_callback};

use crate::{
    config::{DiskFormat, RunArgs},
    disk,
    domain_xml::{self, DomainSpec},
    guest_agent,
    matrix::{self, TestCase},
};

pub fn run(args: RunArgs) -> Result<()> {
    clear_error_callback();

    let matrix = matrix::load_matrix(&args.matrix)?;
    let run_id = Uuid::new_v4().to_string();
    let run_dir = args.workdir.join(&run_id);
    let data_run_dir = args.data_disk_dir.join(&run_id);

    fs::create_dir_all(&run_dir)
        .with_context(|| format!("failed to create workdir {}", run_dir.display()))?;
    fs::create_dir_all(&data_run_dir).with_context(|| {
        format!(
            "failed to create data disk directory {}",
            data_run_dir.display()
        )
    })?;

    let conn = Connect::open(Some(&args.connect_uri))
        .with_context(|| format!("failed to connect to libvirt at {}", args.connect_uri))?;

    eprintln!("[qtr] loaded {} case(s)", matrix.cases.len());

    for case in &matrix.cases {
        let success = run_case(&conn, &args, &run_id, &run_dir, &data_run_dir, case)?;
        if !success {
            bail!("case {} failed", case.name);
        }
    }

    Ok(())
}

fn run_case(
    conn: &Connect,
    args: &RunArgs,
    run_id: &str,
    run_dir: &Path,
    data_run_dir: &Path,
    case: &TestCase,
) -> Result<bool> {
    eprintln!("[qtr] start case: {}", case.name);

    let domain_name = format!("qtr-{}-{}", short_run_id(run_id), case.name);
    let system_disk = run_dir.join(format!("{}-system.qcow2", case.name));
    let data_disk = data_run_dir.join(format!("{}-data.raw", case.name));

    disk::create_overlay(&system_disk, &args.system_base_image, DiskFormat::Qcow2)?;
    disk::create_image(&data_disk, DiskFormat::Raw, &args.data_disk_size)?;

    let xml = domain_xml::build_domain_xml(DomainSpec {
        name: &domain_name,
        memory_mib: args.memory_mib,
        vcpus: args.vcpus,
        system_disk: &system_disk,
        data_disk: &data_disk,
        network: &args.network,
        case,
    });

    let domain = Domain::define_xml(conn, &xml)
        .with_context(|| format!("failed to define domain {domain_name}"))?;

    let case_result = run_defined_domain(&domain, args, &domain_name, case);
    let success = match &case_result {
        Ok(result) => *result,
        Err(_) => false,
    };

    if args.cleanup.should_cleanup(success) {
        cleanup_domain(&domain, &domain_name);
        cleanup_file(&system_disk);
        cleanup_file(&data_disk);
    }

    case_result
}

fn run_defined_domain(
    domain: &Domain,
    args: &RunArgs,
    domain_name: &str,
    case: &TestCase,
) -> Result<bool> {
    domain
        .create()
        .with_context(|| format!("failed to start domain {domain_name}"))?;

    guest_agent::wait_ready(domain, Duration::from_secs(args.agent_timeout_secs))
        .with_context(|| format!("guest agent is not ready for case {}", case.name))?;

    let result = guest_agent::run_command(
        domain,
        &args.test_cmd,
        Duration::from_secs(args.test_timeout_secs),
    )
    .with_context(|| format!("failed to run guest test for case {}", case.name))?;

    io::stdout()
        .write_all(&result.stdout)
        .context("failed to write guest stdout")?;
    io::stdout().flush().context("failed to flush stdout")?;
    io::stderr()
        .write_all(&result.stderr)
        .context("failed to write guest stderr")?;
    io::stderr().flush().context("failed to flush stderr")?;

    if result.exitcode == 0 {
        eprintln!("[qtr] finish case: {} exit=0", case.name);
        Ok(true)
    } else {
        eprintln!("[qtr] failed case: {} exit={}", case.name, result.exitcode);
        Err(anyhow!("guest test exited with {}", result.exitcode))
    }
}

fn cleanup_domain(domain: &Domain, domain_name: &str) {
    match domain.is_active() {
        Ok(true) => {
            if let Err(err) = domain.destroy() {
                eprintln!("[qtr] warning: failed to destroy domain {domain_name}: {err}");
            }
        }
        Ok(false) => {}
        Err(err) => eprintln!("[qtr] warning: failed to check domain {domain_name}: {err}"),
    }

    if let Err(err) = domain.undefine() {
        eprintln!("[qtr] warning: failed to undefine domain {domain_name}: {err}");
    }
}

fn cleanup_file(path: &Path) {
    if let Err(err) = fs::remove_file(path) {
        eprintln!("[qtr] warning: failed to remove {}: {err}", path.display());
    }
}

fn short_run_id(run_id: &str) -> &str {
    run_id.get(..8).unwrap_or(run_id)
}
