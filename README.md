# qtr

QEMU/libvirt based test runner.

## Host Dependencies

Fedora:

```bash
sudo dnf install \
  iscsi-initiator-utils \
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
- `iscsi-initiator-utils`: provides `iscsid` and `iscsiadm` for external block storage.

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

## External Storage

qtr exposes external storage as backends and volumes. iSCSI protocol details are handled internally by the storage driver.

Check host storage prerequisites:

```bash
cargo run -- storage status
```

Register and scan an iSCSI-backed storage service:

```bash
cargo run -- storage add iscsi --name lab-san --address 10.0.0.10
cargo run -- storage scan lab-san
cargo run -- storage volumes lab-san
```

Connect or disconnect a discovered volume:

```bash
cargo run -- storage connect lab-san/db-data-01
cargo run -- storage disconnect lab-san/db-data-01
```

Storage state is written to `.qtr/storage.yaml` by default. Use `--config` to choose another state file.

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

## Declarative VM Config

Generate a starter VM definition:

```bash
cargo run -- vm init --name install-os -o vm.yaml
```

Edit `cdrom` to point at the installer ISO, and create or point `systemDisk` to a qcow2 disk.

```bash
cargo run -- disk create --path .tmp/disks/install-os.qcow2 --format qcow2 --size 40G
```

The generated YAML is an installer-oriented template:

```yaml
name: install-os
systemDisk: .tmp/disks/install-os.qcow2
cdrom: /path/to/installer.iso
boot: [cdrom, hd]
memoryGiB: 4
vcpus: 2
network: default
graphics: vnc
vncListen: 127.0.0.1
serialLog: .tmp/logs/install-os.serial.log
```

Apply it:

```bash
cargo run -- vm apply -f vm.yaml
cargo run -- vm start install-os
cargo run -- vm apply -f vm.yaml --dry-run
cargo run -- vm apply -f vm.yaml --dry-run --color always
```

Dump an existing VM definition:

```bash
cargo run -- vm dump install-os > vm.yaml
cargo run -- vm dump install-os -o vm.yaml
```

`vm dump` writes the supported VM fields from the inactive libvirt domain XML. `vm apply` updates existing VM definitions in place when possible, preserving libvirt-managed XML fields. If the VM is already running, changes take effect on the next start. Relative paths are resolved from the YAML file directory. Use `--dry-run` to print the libvirt domain XML diff without applying it. Diff color defaults to `auto`; use `--color always` or `--color never` to override it.

Run commands through QEMU Guest Agent after the guest boots:

```bash
cargo run -- vm exec install-os -- 'uname -a'
cargo run -- vm exec install-os -- 'journalctl -xb --no-pager'
```
