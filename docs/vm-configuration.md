# VM Configuration

qtr manages virtual machines through declarative YAML definitions and focused lifecycle commands.

## Minimal VM Launch

Generate a starter VM definition, edit `cdroms[].media`, create the disk, then boot the VM:

```bash
cargo run -- vm init --name install-os -o vm.yaml
cargo run -- disk create --path .tmp/disks/install-os.qcow2 --format qcow2 --size 40G
cargo run -- disk info --path .tmp/disks/install-os.qcow2
cargo run -- vm apply -f vm.yaml --start
```

Serial console file output is disabled by default. Configure `serialLog` in the YAML definition to enable it.

## Schema and Capabilities

VM YAML emitted by qtr includes `schemaVersion: 3`. Existing unversioned, version 1, version 2, and standard-VM version 4 definitions remain supported. Version 4 was briefly emitted by a removed VM profile and is read only for compatibility.

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
schemaVersion: 3
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
interfaces:
- id: primary
  type: network
  source: default
  model: virtio
graphics: vnc
vncListen: 127.0.0.1
```

## Machine, CPU, and Memory

`machine.type` is optional; omit it to let libvirt select the machine type. CPU configuration accepts `host-passthrough`, `host-model`, or `custom`. A custom CPU requires `model`. Set either `cpu.vcpus` or `cpu.topology`, but not both.

`memory.sizeMiB` is the current guest memory allocation and optional `memory.maxMiB` sets the maximum allocation. Legacy `vcpus` and `memoryGiB` remain supported for existing definitions but cannot be mixed with their structured replacements.

## Disks

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

### Planned Managed Storage Sources

Current VM disks use a file or host block-device path. The planned Zettide
backend will instead let a disk reference a stable Zettide backend and Volume
identity. qtr will resolve that identity through the managed publication and
iSCSI lifecycle before generating libvirt XML; a transient `/dev/...` path will
remain an implementation detail rather than persisted user configuration.

The future schema must preserve `disks[].id` as the stable guest-device
identity while storing the storage Volume identity separately. Applying an
absent tombstone will detach the guest disk before qtr releases the host session
and Zettide publication. Restart reconciliation will compare all four states:
the VM definition, the Zettide publication, the iSCSI session/device, and the
libvirt attachment.

No managed-storage fields are accepted by the current schema. Continue using a
file or block path and the manual external-storage workflow documented in
[External Storage](external-storage.md).

## CD-ROM Trays

Multiple CD-ROM trays are supported. Set `media: null` to keep an empty tray, change `media` to swap media, or use `state: absent` with the stable ID to remove the tray from the persistent definition. Existing version 1 definitions using the single `cdrom` field remain supported.

CLI `vm apply` changes remain persistent-only while a VM is active; use the [Web API tray operations](web-api.md#installation-media) for live media changes.

## Network Interfaces

Version 3 supports multiple `interfaces`. Each NIC has a stable ID, `network`, `bridge`, or `direct` type, source, model, and optional MAC. IDs are written as `ua-qtr-nic-*` aliases. Use `state: absent` to remove a NIC from the persistent definition. Legacy `network: default` definitions remain supported and only update the first libvirt network interface, leaving additional NICs untouched.

Interfaces also support a single `vlan` tag, `mtu`, and `link: up|down`. VLAN configuration requires `type: bridge` and a VLAN ID from 1 through 4094. Omit these fields to preserve existing XML or set any field to `null` to remove its element.

Use `type: direct` to attach through macvtap, with the host interface in `source` and optional `mode: vepa|bridge|private|passthrough`. VLAN tagging on direct interfaces requires `mode: passthrough`. Omit `mode` to preserve an existing source mode or set it to `null` to remove the XML attribute.

## Apply and Manage VMs

Apply a definition and manage the VM lifecycle:

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

## Dump and Reconcile Definitions

Dump an existing VM definition:

```bash
cargo run -- vm dump install-os > vm.yaml
cargo run -- vm dump install-os -o vm.yaml
cargo run -- vm dump install-os --xml > vm.xml
```

`vm dump` writes the supported VM fields from the inactive libvirt domain XML. Use `--xml` to print the raw inactive libvirt domain XML. `vm apply` updates existing VM definitions in place when possible, preserving libvirt-managed XML fields. Added disks are appended without recreating the VM, and existing disk paths keep their target device when `target` is omitted.

If the VM is already running, changes take effect on the next start. Relative paths are resolved from the YAML file directory. Use `--dry-run` to print the libvirt domain XML diff without applying it. Diff color defaults to `auto`; use `--color always` or `--color never` to override it.

## Guest Agent Commands

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

## Block Devices and IOThreads

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
