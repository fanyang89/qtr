# Getting Started

qtr manages QEMU/KVM virtual machines through the system libvirt connection. This guide prepares a Fedora host and starts the local development environment.

## Host Dependencies

Install the required Fedora packages:

```bash
sudo dnf install \
  edk2-ovmf \
  iscsi-initiator-utils \
  libvirt-client \
  libvirt-daemon \
  libvirt-daemon-driver-qemu \
  libvirt-daemon-config-network \
  libvirt-daemon-kvm \
  libvirt-devel \
  libosinfo \
  openssh-clients \
  osinfo-db \
  pykickstart \
  qemu-img \
  qemu-kvm \
  util-linux-core \
  virt-install \
  pkgconf-pkg-config
```

The important libvirt pieces are:

- `libvirt-daemon-driver-qemu`: provides the `qemu:///system` connection driver.
- `libvirt-daemon-config-network`: provides the default libvirt NAT network config.
- `libvirt-daemon-kvm`: pulls in the KVM/QEMU daemon integration.
- `libvirt-devel`: required to build the Rust `virt` bindings.
- `iscsi-initiator-utils`: provides `iscsid` and `iscsiadm` for external block storage.
- `edk2-ovmf`: provides the EFI firmware used by the QEMU microvm machine.

## Configure libvirt

Start libvirt:

```bash
sudo systemctl enable --now libvirtd
```

Grant the current user access to `qemu:///system` without an interactive polkit agent:

```bash
cargo build
mkdir -p .tmp/disks .tmp/iso
sudo ./target/debug/qtr host setup-libvirt-access \
  --qemu-rw-dir .tmp/disks \
  --qemu-ro-dir .tmp/iso
newgrp libvirt
```

The QEMU system process needs filesystem access to VM media. Use `--qemu-rw-dir` for writable qcow2 disks and optional serial logs, and `--qemu-ro-dir` for readonly ISO directories.

Verify libvirt can load the QEMU driver:

```bash
virsh -c qemu:///system uri
virsh -c qemu:///system list --all
```

Expected `uri` output:

```text
qemu:///system
```

If `virsh` reports `no connection driver available for qemu:///system`, install `libvirt-daemon-driver-qemu` and restart `libvirtd`.

## Development Tools

Local development uses:

- The stable Rust toolchain
- Node.js 22.13 or newer, but earlier than Node.js 25
- pnpm 11.9
- mise for running the API and Vite server together
- Task for repository-wide checks, OpenAPI generation, and packaging

## Run the Development Servers

Install the Web UI dependencies:

```bash
pnpm -C web install --frozen-lockfile
```

Start the Rust API and Vite development server together:

```bash
mise run dev
```

Open <http://127.0.0.1:5173>. Development uses the loopback-only `qtr-dev` token and initializes the browser session automatically. Press `Ctrl+C` to stop both servers.

## CLI Help

Inspect the available commands and command-specific options:

```bash
cargo run -- --help
cargo run -- vm --help
cargo run -- disk --help
cargo run -- storage --help
```

Continue with [VM configuration](vm-configuration.md), [automated Fedora installation](fedora-installation.md), or the [Web UI and API](web-api.md).
