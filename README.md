# qtr

QEMU/libvirt VM manager.

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

## RPM Packaging

Build the RPM package with nFPM:

```bash
task package:rpm
```

The package installs `qtr` to `/usr/bin/qtr` and depends on the libvirt, QEMU, iSCSI, ACL, polkit and systemd packages needed by the CLI. The Web UI is still in development and is not included in the RPM.

After installing the RPM, start libvirt and grant user access for the VM media directories you use:

```bash
sudo systemctl enable --now libvirtd
sudo qtr host setup-libvirt-access \
  --qemu-rw-dir /path/to/disks \
  --qemu-ro-dir /path/to/iso
newgrp libvirt
```

Deploy the RPM to Fedora 44 hosts with the uv-managed Ansible playbook:

```bash
uv sync
cp deploy/inventory.example.ini deploy/inventory.ini
task deploy:rpm LIMIT=fedora44
```

The deploy playbook copies the local RPM to the target, installs it with `dnf5`, and enables `libvirtd`.

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

Generate a starter VM definition, edit `cdrom`, create the disk, then boot the VM:

```bash
cargo run -- vm init --name install-os -o vm.yaml
cargo run -- disk create --path .tmp/disks/install-os.qcow2 --format qcow2 --size 40G
cargo run -- disk info --path .tmp/disks/install-os.qcow2
cargo run -- vm apply -f vm.yaml --start
```

Serial console file output is disabled by default. Configure `serialLog` in the YAML definition to enable it.

## Declarative VM Config

VM YAML emitted by qtr includes `schemaVersion: 2`. Existing unversioned and version 1 definitions remain supported.

Query the VM features reported by the current libvirt/QEMU host before using host-specific machine, firmware, CPU or device options:

```bash
cargo run -- vm capabilities
cargo run -- vm capabilities --machine q35 --json
```

Generate a starter VM definition:

```bash
cargo run -- vm init --name install-os -o vm.yaml
cargo run -- vm init --name install-os --machine q35 -o vm.yaml
```

Edit `cdroms[].media` to point at the installer ISO. Create or resize disks with `disk` commands before applying the VM definition.

The generated YAML is an installer-oriented template:

```yaml
schemaVersion: 2
name: install-os
machine:
  type: q35
cpu:
  mode: host-passthrough
  topology:
    sockets: 1
    cores: 2
    threads: 1
memory:
  sizeMiB: 4096
disks:
- id: disk0
  path: .tmp/disks/install-os.qcow2
  type: file
  format: qcow2
cdroms:
- id: installer
  media: /path/to/installer.iso
boot: [cdrom, hd]
network: default
graphics: vnc
vncListen: 127.0.0.1
```

`machine.type` is optional; omit it to let libvirt select the machine type. CPU configuration accepts `host-passthrough`, `host-model`, or `custom`. A custom CPU requires `model`. Set either `cpu.vcpus` or `cpu.topology`, but not both. `memory.sizeMiB` is the current guest memory allocation and optional `memory.maxMiB` sets the maximum allocation. Legacy `vcpus` and `memoryGiB` remain supported for existing definitions but cannot be mixed with their structured replacements.

`disks[].id` is the stable qtr device identity and is written to libvirt as a `ua-qtr-disk-*` alias. Keep the ID unchanged when moving a disk source or reordering disks. Existing definitions without IDs remain supported; `vm dump` derives deterministic IDs from disk targets.

Disks support `discard: ignore|unmap`, `detectZeroes: off|on|unmap`, `readonly`, and a guest-visible `serial`. Omitted advanced fields preserve existing libvirt XML during updates. Set `readonly: false` to make an existing disk writable and `serial: null` to remove its serial. `discard: unmap` only releases storage when the complete backing chain supports it; disk `serial` is separate from the VM console `serialLog`.

Per-disk `ioTune` supports `totalBytesPerSec`, `readBytesPerSec`, `writeBytesPerSec`, `totalIops`, `readIops`, and `writeIops`. A non-zero total limit cannot be combined with non-zero read/write limits in the same category. Zero disables an individual limit. Omit `ioTune` to preserve existing XML or set `ioTune: null` to remove all limits.

Persistently detach a disk by keeping its ID as an absent tombstone:

```yaml
disks:
- id: root
  path: .tmp/disks/install-os.qcow2
  format: qcow2
- id: old-data
  state: absent
```

Detach updates only the inactive libvirt domain definition. It never deletes the disk file, block device, or storage volume. Omitting an existing disk without `state: absent` remains an error.

Multiple CD-ROM trays are supported. Set `media: null` to keep an empty tray, change `media` to swap media, or use `state: absent` with the stable ID to remove the tray from the persistent definition. Existing version 1 definitions using the single `cdrom` field remain supported.

Apply it:

```bash
cargo run -- vm apply -f vm.yaml
cargo run -- vm start install-os
cargo run -- vm stop install-os --wait
cargo run -- vm reboot install-os
cargo run -- vm reset install-os
cargo run -- vm suspend install-os
cargo run -- vm resume install-os
cargo run -- vm autostart install-os
cargo run -- vm autostart install-os --enable
cargo run -- vm autostart install-os --disable
cargo run -- vm save install-os
cargo run -- vm saved-state install-os
cargo run -- vm restore install-os
cargo run -- vm saved-state install-os --remove
cargo run -- vm disk-resize install-os root 64GiB
cargo run -- vm rm install-os
cargo run -- vm apply -f vm.yaml --dry-run
cargo run -- vm apply -f vm.yaml --dry-run --color always
```

`reboot` requests an orderly guest reboot through libvirt. `reset` is an immediate virtual hardware reset and can cause data loss. `suspend` and `resume` are idempotent for already paused/running VMs. `autostart` without a flag prints the current state. `save` stores the running state as a libvirt managed save image and stops the VM. `restore` starts an inactive VM from that image. `saved-state` prints `present` or `absent`; pass `--remove` to discard the image without starting the VM.

`disk-resize` accepts a stable disk ID or target such as `vda` and an absolute capacity in bytes or binary units. It only permits expansion. Running VMs use libvirt block resize; inactive file-backed disks use `qemu-img`. Inactive block-backed disks must be expanded through their storage backend first. The command changes virtual disk capacity only; grow guest partitions and filesystems separately.

Dump an existing VM definition:

```bash
cargo run -- vm dump install-os > vm.yaml
cargo run -- vm dump install-os -o vm.yaml
cargo run -- vm dump install-os --xml > vm.xml
```

`vm dump` writes the supported VM fields from the inactive libvirt domain XML. Use `--xml` to print the raw inactive libvirt domain XML. `vm apply` updates existing VM definitions in place when possible, preserving libvirt-managed XML fields. Added disks are appended without recreating the VM, and existing disk paths keep their target device when `target` is omitted. If the VM is already running, changes take effect on the next start. Relative paths are resolved from the YAML file directory. Use `--dry-run` to print the libvirt domain XML diff without applying it. Diff color defaults to `auto`; use `--color always` or `--color never` to override it.

Run commands through QEMU Guest Agent after the guest boots:

```bash
cargo run -- vm exec install-os -- uname -a
cargo run -- vm exec install-os -- journalctl -xb --no-pager
cargo run -- vm exec install-os --script examples/kworker-fio.sh --output results/native-none.json
cargo run -- vm cp install-os ./fio.conf guest:/tmp/fio.conf
cargo run -- vm cp install-os guest:/tmp/qtr-fio.json ./results/qtr-fio.json --parents
```

Each `vm exec` argument is passed to the guest verbatim; quoting and expansion happen only in your local shell. For guest-side shell features such as pipes, redirection or variable expansion, invoke a shell explicitly:

```bash
cargo run -- vm exec install-os -- sh -c 'systemctl --failed | head'
```

`vm exec --script` uploads a local script to a temporary guest path, runs it with `/bin/sh`, streams stdout/stderr, then removes the guest copy. `--output` disables streaming and writes a JSON result with exit code, elapsed time, stdout and stderr. The guest needs QEMU Guest Agent running.

If `vm exec` reports that `guest-exec` is disabled, enable the `guest-exec` RPC in the guest's `qemu-ga` configuration. Remove it from `block-rpcs`/`blacklist`, or add it to `allow-rpcs`, then restart `qemu-guest-agent` inside the guest.

`vm cp` copies one file between host and guest. Prefix the guest path with `guest:`. Exactly one side must be a guest path. Use `--parents` to create the destination parent directory.

Use `type: block` in `disks` to pass a host block device directly to the guest:

```yaml
ioThreads:
  count: 4
  queues: 4
disks:
- path: .tmp/disks/install-os.qcow2
  type: file
  format: qcow2
  bus: virtio-blk
- path: /dev/disk/by-id/example-block-device
  type: block
  format: raw
  target: sda
  bus: virtio-scsi
  cache: none
  io:
    mode: threads
```

Prefer stable `/dev/disk/by-id/...` paths, ensure the host is not using the device, and expose it as a non-boot disk such as `target: sda` for performance tests. `ioThreads.count` configures VM-level QEMU IOThreads; `ioThreads.queues` defaults to the same value. Use `io.mode: threads` on `virtio-blk` or `virtio-scsi` disks to attach them to the IOThread mapping.
