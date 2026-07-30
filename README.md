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

## Web API

Start the Rust API and Vite development server together:

```bash
mise run dev
```

Open `http://127.0.0.1:5173`. Development uses the loopback-only `qtr-dev` token and initializes the browser session automatically. Press `Ctrl+C` to stop both servers.

For a production-style build, build the Web UI and start the authenticated server manually:

```bash
pnpm -C web build
openssl rand -hex 32 > .qtr-api-token
cargo run -- web --api-token-file .qtr-api-token
```

Open `http://127.0.0.1:8080/access` and store the token for the current browser tab. Management endpoints are under `/api/v1`; `/api/v1/health`, `/api/v1/openapi.json`, and `/docs` are public. VNC connections use short-lived, single-use tickets issued by the authenticated API.

Fedora installations are persistent jobs under `/api/v1/install-jobs`. Requests use `mediaId` and `imageId` instead of host paths. The default roots are `.tmp/iso`, `.tmp/disks`, and `.tmp/logs`; server state and VM manifests are stored under `.qtr/server`. Override them with `--media-root`, `--image-root`, `--log-root`, and `--state-dir`. SQLite uses WAL mode, queued jobs resume after restart, and jobs that were running are marked `interrupted` without deleting uncertain VM resources.

The ISOs page supports authenticated, streaming upload and protected deletion. Uploads are limited to 32 GiB by default; override the limit with `--max-iso-upload-bytes`. Existing ISO IDs are never overwritten, and deletion is blocked while an ISO is referenced by an automated install or VM CD-ROM.

The Disks page creates blank managed raw and qcow2 images without host shell access. Image IDs must use the extension matching their format, and creation is rejected while an automated install reserves the same ID.

qtr serves plain HTTP. Keep the default loopback binding or put a TLS reverse proxy in front of qtr on trusted networks. The server logs a warning when plain HTTP listens on a non-loopback address.

Regenerate the committed OpenAPI 3.1 document after changing API handlers:

```bash
task openapi:generate
```

## RPM Packaging

Build the RPM package with nFPM:

```bash
task package:rpm
```

The package includes the qtr binary, built Web UI, systemd unit, service user declaration, and persistent directory layout. The service listens on `127.0.0.1:8080` and expects its API token at `/etc/qtr/api-token`.

After installing the RPM, create the service token and start qtr:

```bash
sudo systemctl enable --now libvirtd
QTR_TOKEN="$(openssl rand -hex 32)"
printf '%s\n' "$QTR_TOKEN" | sudo tee /etc/qtr/api-token >/dev/null
sudo chown root:qtr /etc/qtr/api-token
sudo chmod 0640 /etc/qtr/api-token
sudo systemctl enable --now qtr
```

Open `/access` through a TLS reverse proxy and enter `QTR_TOKEN`. The proxy must support WebSocket upgrades for the VNC console. qtr remains loopback-only by default; do not expose port 8080 directly.

Deploy the RPM to Fedora 44 hosts with the uv-managed Ansible playbook:

```bash
uv sync
cp deploy/inventory.example.ini deploy/inventory.ini
task deploy:rpm LIMIT=fedora44
```

The deploy playbook copies the local RPM, installs it with `dnf5`, creates the API token when absent, enables `libvirtd` and qtr, then waits for the health endpoint.

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

## Automated Fedora Installation

Install Fedora Server from a local x86_64 DVD ISO:

```bash
cargo run -- vm install fedora \
  --name fedora44 \
  --iso .tmp/iso/Fedora-Server-dvd-x86_64-44.iso \
  --disk .tmp/disks/fedora44.qcow2 \
  --disk-size 40GiB \
  --output .tmp/fedora44.yaml \
  --ssh-key ~/.ssh/id_ed25519.pub
```

The installer creates a minimal Btrfs system with a locked root account and a `qtr` sudo user. It disables SELinux with the `selinux=0` kernel argument, masks firewalld, installs and enables `qemu-guest-agent`, then starts and verifies the installed VM. Pass `--mirror tuna` to configure the installed Fedora repositories to use TUNA; installation itself uses the local DVD.

The disk and output parent directories must already exist. The ISO, disk, output YAML, serial log and install log paths must be accessible as appropriate. Use `host setup-libvirt-access` for the containing directories. Failed installs are rolled back before the final XML is committed; pass `--keep-failed` to preserve diagnostic resources.

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

VM YAML emitted by qtr includes `schemaVersion: 3`. Existing unversioned, version 1, and version 2 definitions remain supported.

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

Version 3 supports multiple `interfaces`. Each NIC has a stable ID, `network` or `bridge` type, source, model, and optional MAC. IDs are written as `ua-qtr-nic-*` aliases. Use `state: absent` to remove a NIC from the persistent definition. Legacy `network: default` definitions remain supported and only update the first libvirt network interface, leaving additional NICs untouched.

Interfaces also support a single `vlan` tag, `mtu`, and `link: up|down`. VLAN configuration requires `type: bridge` and a VLAN ID from 1 through 4094. Omit these fields to preserve existing XML or set any field to `null` to remove its element.

Use `type: direct` to attach through macvtap, with the host interface in `source` and optional `mode: vepa|bridge|private|passthrough`. VLAN tagging on direct interfaces requires `mode: passthrough`. Omit `mode` to preserve an existing source mode or set it to `null` to remove the XML attribute.

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
