# qtr

QEMU/libvirt based test runner.

## Host Dependencies

Fedora:

```bash
sudo dnf install \
  libvirt-client \
  libvirt-daemon \
  libvirt-daemon-driver-qemu \
  libvirt-daemon-config-network \
  libvirt-daemon-kvm \
  libvirt-devel \
  qemu-img \
  qemu-kvm \
  pkgconf-pkg-config
```

The important libvirt pieces are:

- `libvirt-daemon-driver-qemu`: provides the `qemu:///system` connection driver.
- `libvirt-daemon-config-network`: provides the default libvirt NAT network config.
- `libvirt-daemon-kvm`: pulls in the KVM/QEMU daemon integration.
- `libvirt-devel`: required to build the Rust `virt` bindings.

Start libvirt:

```bash
sudo systemctl enable --now libvirtd
```

Grant the current user access to `qemu:///system` without an interactive polkit agent:

```bash
cargo build
mkdir -p .tmp/disks .tmp/iso .tmp/logs
sudo ./target/debug/qtr host setup-libvirt-access \
  --qemu-rw-dir .tmp/disks \
  --qemu-rw-dir .tmp/logs \
  --qemu-ro-dir .tmp/iso
newgrp libvirt
```

The QEMU system process needs filesystem access to VM media. Use `--qemu-rw-dir` for writable qcow2 disks and serial logs, and `--qemu-ro-dir` for readonly ISO directories.

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

## Minimal VM Launch

Create a qcow2 system disk and boot an installer ISO with VNC exposed:

```bash
cargo run -- vm launch \
  --name install-os \
  --system-disk .tmp/disks/sys.qcow2 \
  --create-system-disk 40G \
  --cdrom .tmp/iso/CentOS-7-x86_64-Everything-2207-02.iso \
  --boot cdrom,hd \
  --vnc-listen 0.0.0.0
```

`vm create` only defines a VM. Use `vm launch`, or run `vm start` after `vm create`.

VMs write serial console output to `.tmp/logs/<name>.serial.log` by default. Override it with `--serial-log`.

Run commands through QEMU Guest Agent after the guest boots:

```bash
cargo run -- vm exec install-os -- 'uname -a'
cargo run -- vm exec install-os -- 'journalctl -xb --no-pager'
```
