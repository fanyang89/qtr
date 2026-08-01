<div align="center">

# qtr

**A self-hosted QEMU/libvirt virtual machine manager.**

[![CI](https://github.com/fanyang89/qtr/actions/workflows/ci.yml/badge.svg)](https://github.com/fanyang89/qtr/actions/workflows/ci.yml)
![Rust 2024](https://img.shields.io/badge/Rust-2024-000000?logo=rust)
![Platform](https://img.shields.io/badge/platform-Linux-blue)

</div>

qtr combines a command-line interface, authenticated Web UI, and REST API for managing Linux virtual machines on `qemu:///system`.

## Features

- Declarative YAML definitions for VM, disk, network, CPU, and memory configuration
- VM lifecycle operations, managed save state, disk resize, and QEMU Guest Agent commands
- Browser dashboard with authenticated management APIs and ticketed noVNC consoles
- Managed disk images, installation media, and live CD-ROM tray operations
- Automated, unattended Fedora Server installation
- External iSCSI storage discovery and host connections
- RPM, systemd, and Ansible deployment support for Fedora hosts

## Quick Start

qtr requires a Linux host with QEMU/KVM and libvirt. Complete the [Fedora host setup](docs/getting-started.md), then install the Web UI dependencies and start both development servers:

```bash
pnpm -C web install --frozen-lockfile
mise run dev
```

Open <http://127.0.0.1:5173>. The development environment uses a loopback-only token and initializes the browser session automatically.

To explore the CLI:

```bash
cargo run -- --help
cargo run -- vm --help
```

## Documentation

| Guide | Description |
| --- | --- |
| [Documentation index](docs/README.md) | All user and operator guides |
| [Getting started](docs/getting-started.md) | Fedora dependencies, libvirt access, and local development |
| [Web UI and API](docs/web-api.md) | Authentication, managed resources, server state, and OpenAPI |
| [VM configuration](docs/vm-configuration.md) | Declarative schema, lifecycle, disks, networking, and guest operations |
| [Automated Fedora installation](docs/fedora-installation.md) | Build a Fedora VM from a local DVD ISO |
| [External storage](docs/external-storage.md) | Register, scan, and connect iSCSI-backed volumes |
| [Deployment](docs/deployment.md) | Build and deploy the RPM with systemd and Ansible |

The frontend-specific development notes are in [`web/README.md`](web/README.md). The committed OpenAPI 3.1 document is [`openapi/qtr-v1.json`](openapi/qtr-v1.json).

## Zettide Storage Direction

The current storage commands discover iSCSI targets, log the host in or out,
and report the resulting Linux block devices. VM definitions still refer to a
local file or block-device path, so this is a manual external-storage workflow,
not a managed attachment lifecycle.

The target Zettide integration adds a managed backend keyed by stable Zettide
Volume IDs. qtr will publish a Volume for the selected host, establish and
persist the iSCSI session, validate its expected SCSI serial or WWID, resolve
the current device path, attach it to libvirt, and reconcile those steps after
restart. Detach will remove the VM attachment before releasing the session and
publication. This backend and its VM schema do not exist yet.

The first integration targets a single Zettide storage node. A later
distributed tier keeps the same qtr identity and attachment contract while
Zettide handles storage failover and allows the Volume to be republished to a
specified qtr host. VM scheduling and automatic VM restart remain separate
compute-layer concerns.

## Development

Run the complete local verification suite:

```bash
task check
```

After changing API handlers, regenerate the OpenAPI document:

```bash
task openapi:generate
```

## Security

qtr serves plain HTTP and binds to loopback by default. Keep it loopback-only or place a TLS reverse proxy with WebSocket support in front of it on a trusted network. Do not expose the qtr service port directly to an untrusted network.
