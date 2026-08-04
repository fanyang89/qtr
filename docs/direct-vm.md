# Direct VMs

The experimental `direct-vm` command runs Cloud Hypervisor as a process owned by qtr. Direct VMs use separate manifests and state from libvirt VMs.

## Scope

The initial backend supports:

- UEFI firmware boot
- raw and qcow2 disk files supported by Cloud Hypervisor
- fixed vCPU and memory allocation
- unprivileged user networking with DHCP, DNS, and TCP/UDP port forwarding
- an optional pre-created TAP interface for advanced networking
- serial and VMM logs
- define, list, show, start, stop, and remove operations

It does not support the Web API, automated installation, VNC, Guest Agent commands, hotplug, or autostart.

## Prerequisites

Install a Cloud Hypervisor release and obtain firmware built for Cloud Hypervisor. The process must be able to access `/dev/kvm`, the firmware, and disks.

User networking requires `passt` with vhost-user support and Cloud Hypervisor 52 or newer. It runs without root privileges and does not modify host interfaces or firewall rules.

This integration remains experimental: `passt` currently documents QEMU as its supported hypervisor, while Cloud Hypervisor interoperability relies on its compatible vhost-user-net implementation.

Pass non-default executables when required:

```bash
cargo run -- direct-vm \
  --cloud-hypervisor /opt/cloud-hypervisor/bin/cloud-hypervisor \
  --passt /usr/bin/passt \
  list
```

## Manifest

```yaml
schemaVersion: 2
name: edge-worker
firmware: /opt/cloud-hypervisor/share/hypervisor-fw
cpus: 2
memoryMiB: 1024
disks:
- path: /var/lib/qtr/images/edge-worker.qcow2
  readonly: false
network:
  type: user
  forwards:
  - protocol: tcp
    hostPort: 2222
    guestPort: 22
```

The user network provides outbound TCP, UDP, ICMP, DHCP, and DNS through `passt`. qtr discovers a non-loopback upstream resolver from systemd-resolved or `/etc/resolv.conf`; startup fails with an actionable error if neither provides one. Port forwards bind to `127.0.0.1` by default; set `hostAddress` explicitly to expose a service on another host address. Host and guest ports must be individual non-zero ports. qtr generates and persists a locally administered MAC address when `mac` is omitted.

Omit `network` for an offline VM. Existing schema v1 manifests remain readable and are converted to schema v2 in memory.

For direct Layer 2 connectivity, create and configure a TAP interface before starting the VM:

```yaml
network:
  type: tap
  tap: tap-edge-worker
  mac: 02:00:00:00:00:01
```

qtr validates a configured TAP interface but does not create, attach, or remove it. Firmware and disk paths are resolved and persisted as canonical paths when the VM is defined.

## Lifecycle

```bash
cargo run -- direct-vm define --file edge-worker.yaml
cargo run -- direct-vm list
cargo run -- direct-vm show edge-worker
cargo run -- direct-vm start edge-worker
cargo run -- direct-vm stop edge-worker
cargo run -- direct-vm rm edge-worker
```

The default state root is `.qtr/direct-vms`. State directories and files are private to the creating user. Each VM directory contains its normalized manifest, API socket, PID file, serial log, and VMM log. User networking also adds a `passt` PID, socket, and log.

`stop` requests `vm.power-button` over the Cloud Hypervisor Unix API socket and waits for the guest to shut down. If the guest does not exit, retry with `--force` to request `vmm.shutdown`. qtr never signals the VMM PID directly. It can terminate an orphaned `passt` process only after pinning the PID with pidfd and verifying its exact socket arguments, an active matching socket, and socket ownership when the sandbox permits inspection.

## Recovery

A missing process with a remaining PID file is treated as stopped only when no process owns the corresponding socket. A live PID whose command line or socket ownership does not match the expected runtime is reported as `stale`; qtr will not control or remove it. An active socket without trusted process metadata is reported as `untracked`; an inactive socket file is cleaned during the next lifecycle operation.

For user networking, `degraded` means the VMM is running but its network sidecar is not verified healthy. `orphaned-network` means the VMM stopped while a verified `passt` process remained running. Both states block unsafe restart or removal; `stop` safely terminates a verified orphan before cleaning its runtime files.

Cloud Hypervisor exits before creating its API socket are reported with the VMM log path. qtr does not currently restart crashed direct VMs automatically.
