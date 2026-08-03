# Direct VMs

The experimental `direct-vm` command runs Cloud Hypervisor as a process owned by qtr. Direct VMs use separate manifests and state from libvirt VMs.

## Scope

The initial backend supports:

- UEFI firmware boot
- raw and qcow2 disk files supported by Cloud Hypervisor
- fixed vCPU and memory allocation
- an optional pre-created TAP interface
- serial and VMM logs
- define, list, show, start, stop, and remove operations

It does not support the Web API, automated installation, VNC, Guest Agent commands, hotplug, autostart, or managed networking.

## Prerequisites

Install a Cloud Hypervisor release and obtain firmware built for Cloud Hypervisor. The process must be able to access `/dev/kvm`, the firmware, disks, and any configured TAP interface.

Pass a non-default executable when required:

```bash
cargo run -- direct-vm \
  --cloud-hypervisor /opt/cloud-hypervisor/bin/cloud-hypervisor \
  list
```

Create and configure TAP devices before starting a VM. qtr validates the interface but does not create, attach, or remove it.

## Manifest

```yaml
schemaVersion: 1
name: edge-worker
firmware: /opt/cloud-hypervisor/share/hypervisor-fw
cpus: 2
memoryMiB: 1024
disks:
- path: /var/lib/qtr/images/edge-worker.qcow2
  readonly: false
network:
  tap: tap-edge-worker
  mac: 02:00:00:00:00:01
```

Firmware and disk paths are resolved and persisted as canonical paths when the VM is defined.

## Lifecycle

```bash
cargo run -- direct-vm define --file edge-worker.yaml
cargo run -- direct-vm list
cargo run -- direct-vm show edge-worker
cargo run -- direct-vm start edge-worker
cargo run -- direct-vm stop edge-worker
cargo run -- direct-vm rm edge-worker
```

The default state root is `.qtr/direct-vms`. State directories and files are private to the creating user. Each VM directory contains its normalized manifest, API socket, PID file, serial log, and VMM log.

`stop` requests `vm.power-button` over the Cloud Hypervisor Unix API socket and waits for the guest to shut down. If the guest does not exit, retry with `--force` to request `vmm.shutdown`. qtr does not signal a PID read from persistent state.

## Recovery

A missing process with a remaining PID file is treated as stopped and cleaned during the next lifecycle operation. A live PID whose command line does not contain the expected API socket is reported as `stale`; qtr will not control or remove it. An API socket that responds without a tracked process is reported as `untracked`, preventing another VMM from starting against the same disks.

Cloud Hypervisor exits before creating its API socket are reported with the VMM log path. qtr does not currently restart crashed direct VMs automatically.
