# Automated Fedora Installation

qtr can install Fedora Server from a local x86_64 DVD ISO with an unattended Kickstart configuration.

## Install a VM

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

## Filesystem Access and Failure Handling

The disk and output parent directories must already exist. The ISO, disk, output YAML, serial log and install log paths must be accessible as appropriate. Use `host setup-libvirt-access` for the containing directories as described in [Getting started](getting-started.md#configure-libvirt).

Failed installs are rolled back before the final XML is committed. Pass `--keep-failed` to preserve diagnostic resources.

The Web API also exposes installations as persistent jobs. See [Install jobs and server state](web-api.md#install-jobs-and-server-state) for their restart and cancellation behavior.
