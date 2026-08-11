# Kernel ACPI Idle E2E

This harness runs one kernel RPM, CPU profile, and expected idle-driver result in an isolated libvirt VM. An outer job can invoke it repeatedly to build a matrix.

It always uses `pc-i440fx-10.2` and a uniquely named qcow2 overlay. The base image is only attached as the overlay's read-only backing file. Cleanup addresses only the unique `qtr-acpi-*` domain and its overlay; it does not enumerate or modify other VMs such as `fedora44-e2e`.

## Inputs

Required environment variables:

- `QTR_BASE_IMAGE`: local raw or qcow2 guest image.
- `QTR_KERNEL_RPM`: local, self-contained kernel RPM that installs `/boot/vmlinuz-<release>` and `/lib/modules/<release>`.
- `QTR_EXPECTED_DRIVER`: `acpi_idle` or `none`.
- `QTR_CPU_PROFILE`: `hygon-baremetal`, `hygon-kvm`, or `native-baremetal`.

Optional environment variables:

- `QTR_ARTIFACT_DIR`: output directory. The default is `.tmp/artifacts/kernel-acpi-idle/<run-id>-<profile>-<driver>`.
- `QTR_BIN`: qtr executable. The default is `target/debug/qtr`.

The Hygon profiles use the custom `Dhyana` model and disable `rdseed`. `hygon-baremetal` also disables `hypervisor`; `hygon-kvm` leaves it exposed. `native-baremetal` uses host passthrough and disables `hypervisor`.

## Prerequisites

The host needs KVM, system libvirt, QEMU with `pc-i440fx-10.2` and `Dhyana` support when a Hygon profile is selected, `qemu-img`, `virsh`, passwordless `sudo` for `qtr host fix-vm-perms`, and permission to manage `qemu:///system`. The base image and its parent directories must already be readable/searchable by the `qemu` user; the harness checks this without changing the base image ACL. Build the current qtr commit before running directly:

```bash
cargo build
```

The prepared base guest must already contain these offline dependencies:

- `rpm`
- `dracut`
- `grubby`
- `python3`
- `cpio` (GNU cpio with `--reproducible`)
- `qemu-guest-agent`, running with the `guest-exec` and guest file RPCs enabled

No guest network interface is attached and no command installs dependencies from a repository.

## Run

```bash
QTR_BASE_IMAGE=/var/lib/libvirt/images/fedora-base.qcow2 \
QTR_KERNEL_RPM=/path/to/kernel-6.x.y.rpm \
QTR_CPU_PROFILE=hygon-baremetal \
QTR_EXPECTED_DRIVER=acpi_idle \
task e2e:kernel-acpi-idle
```

Run one process at a time for a given host capacity. A matrix driver can vary `QTR_KERNEL_RPM`, `QTR_CPU_PROFILE`, `QTR_EXPECTED_DRIVER`, and `QTR_ARTIFACT_DIR` between invocations.

## ACPI Construction

The first guest boot reads QEMU's active DSDT and FADT from sysfs. `aml.py` validates each ACPI header, exact table length, and checksum. It rejects `_CST`, strictly decodes every AML `ProcessorOp` package and NameString, requires every original P_BLK address and length to be `0/0`, and requires FADT C2 latency greater than 100 and C3 latency greater than 1000.

The patch sets every P_BLK address to `0xDEAD0000` and length to 6, increments the OEM revision, and recalculates the checksum. The address is deliberately outside the x86 I/O-port range. It is only a nonzero discovery placeholder: the validated FADT latencies force the kernel to discard C2/C3, and the post-boot probe rejects any exposed C2/C3 state.

The patched DSDT is stored as `kernel/firmware/acpi/DSDT` in an uncompressed, deterministic `newc` archive. That archive is prepended to a newly generated dracut image before `grubby` creates and selects the target boot entry. Preparation fails unless the target kernel has `CONFIG_ACPI_TABLE_UPGRADE=y`.

After `qtr vm reboot`, the harness waits for a changed boot ID and verifies the target `uname -r`, exact overridden DSDT hash/revision/ProcessorOps, invalid FADT idle latencies, CPU vendor and feature flags, idle driver, and idle states. `acpi_idle` must expose `C1` with `ACPI HLT` and no C2/C3. `none` accepts an unreadable/missing `current_driver` or the literal value `none`.

## Artifacts

Artifacts include the generated VM manifest, qtr and active/final domain XML, serial log, run log, capabilities, base-image metadata, RPM preparation output, patched DSDT and metadata, boot IDs, guest probe JSON, dmesg, and cleanup logs. On failure, the harness also captures a guest CPU/idle probe, boot journal, final dmesg, domain state, and live XML before cleanup.
