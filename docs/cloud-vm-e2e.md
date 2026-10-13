# Real Cloud Guest E2E

`task e2e:cloud-vm` exercises a real Ubuntu cloud guest through a running qtr
REST API. It is separate from the fast, empty-disk `task e2e:libvirt` test.

## Manual GitHub Actions run

Run the **CI** workflow with the `cloud-vm` input enabled. The `setup-action`
job builds the current checkout, starts it using the local setup action, then
runs the cloud test. Ordinary push and pull-request runs do not download or
boot a cloud image. Harness unit tests run on every CI invocation.

The test checks:

1. Import a checksum-verified base image and create a linked clone.
2. Generate NoCloud seed media with an ephemeral SSH public key.
3. Create and start a VM through the REST API.
4. Wait for Guest Agent addresses, SSH, and successful cloud-init completion.
5. Request an orderly reboot and observe a **different guest boot ID** before
   requiring readiness again.
6. Delete the VM, seed, and overlay; verify that the base image has not changed;
   delete the base and verify resource absence.

## Local use

Use a **dedicated local test server**, with its libvirt connection set to
`qemu:///system`. Do not point this test at a production service. It requires
KVM, an active `default` libvirt network with DHCP/NAT, host-to-guest SSH access,
and guest Internet access to install `qemu-guest-agent`. The server needs its
normal cloud-init/image tools and QEMU access to its managed resource roots.
The test client needs Python 3.11+, OpenSSH (`ssh` and `ssh-keygen`), and GNU
`timeout`; it adds no Python dependencies.

Export the same variables supplied by the setup action:

```bash
export QTR_API_URL=http://127.0.0.1:8080/api/v1
export QTR_API_TOKEN_FILE=/path/to/private/api-token
export QTR_IMAGE_ROOT=/path/to/test/images
export QTR_MEDIA_ROOT=/path/to/test/media
export QTR_LOG_ROOT=/path/to/test/logs
export QTR_CLOUD_SERVICE_LOG=/path/to/test/qtr.log  # optional diagnostics

task e2e:cloud-vm
```

Resource IDs contain a random per-run suffix. The test never searches or deletes
by prefix. Cleanup records creation intent before requests, so a lost response
still leads to a cleanup attempt; a rejected creation does not claim an existing
resource. Failed deletions remain tracked and make the test fail.

`TERM` and ordinary errors attempt cleanup. An uncatchable kill or unavailable
server can still leave resources; use the exact IDs in `resources.json` for
manual recovery. This is not a persistent lease or a resource ownership API.

## Image and diagnostics

The pinned image is Ubuntu 24.04 amd64, release `20260926`, from
<https://cloud-images.ubuntu.com/releases/noble/release-20260926/>. Its SHA-256
is committed in `e2e/cloud-vm/run.py` from the upstream `SHA256SUMS`. Updating the
image requires reviewing both the version and upstream checksum.

Images are cached by checksum in `.tmp/cloud-images` and reverified on use.
Set `QTR_CLOUD_CACHE` to change the cache directory. `QTR_CLOUD_IMAGE_URL` accepts
an HTTPS mirror of the **same bytes**, never a checksum override. On mainland
China hosts, prefer a reputable domestic mirror that carries this exact release;
fall back to upstream if unavailable. The disposable guest is a test fixture,
not a production image or a fully pinned guest package environment.

The overall command has a 20-minute deadline; individual API, SSH and readiness
operations are also bounded. Cleanup receives a 90-second grace period after
TERM. Artifacts default to `.tmp/e2e/cloud-vm`, configurable through
`QTR_E2E_ARTIFACT_DIR`. CI uploads them even on failure: guest status, boot IDs,
serial and cloud-init logs where available, resource IDs, and the optional qtr
service log. Private SSH keys and API token files are never copied to artifacts.
SSH uses a per-run known-hosts file, not the user's SSH trust store.

Test the harness without KVM, network downloads, or a qtr server:

```bash
task e2e:cloud-vm:test
```
