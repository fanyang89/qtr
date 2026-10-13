# Three-node Jepsen adapter smoke test

`task e2e:jepsen` exercises the real qtr Jepsen adapter against three ephemeral
cloud VMs. It uses the adapter's provisioning API, Jepsen's SSHJ control command
path, and its `Nemesis` protocol. It does **not** run a database workload,
`jepsen.core/run!`, or a consistency checker. Passing this test proves the adapter
workflow, not database correctness or distributed-system fault tolerance.

## Coverage

The main scenario creates a managed run with `n1`, `n2`, and `n3`, each with one
vCPU and 1024 MiB RAM:

1. Import a verified cloud image, then provision three linked clones and seeds
   through `qtr/provision!` with `:managed-run? true`.
2. Confirm three distinct Guest Agent IPv4 addresses and the Jepsen node order.
3. Authenticate over SSH, wait for cloud-init, verify hostnames, and record boot IDs.
4. Check TCP/22 connectivity for all six directed guest-to-guest pairs.
5. Suspend `n1`, observe `paused`, verify the other guests still respond, then
   resume `n1` without a boot ID change.
6. Reset exactly `n2` and `n3`; require new boot IDs and restored SSH on both,
   while `n1` retains its boot ID.
7. Leave `n1` suspended and require nemesis teardown to resume it.
8. Close the run, verify the exact nine resource records are deleted, verify the
   VMs and managed files are absent, and repeat cleanup to check idempotence.

A second run deliberately throws while preparing `n2`, **after `n1` is ready**.
The adapter must roll back the first node's three resources. The journal must be
non-empty and closed, so a failure before any provisioning cannot falsely pass.
Only one VM is created in this second scenario; the peak remains three guests.

The launcher finally verifies that the separately imported base image still has
its upstream SHA-256, then removes that temporary import. The cached source image
is never modified or deleted.

## Manual CI

Run the **CI** workflow with `jepsen-smoke` enabled. The `setup-action` job builds
the checkout, starts qtr using the local action, and runs the smoke test. The
existing `cloud-vm` input remains independent; either or both may be enabled.
Ordinary push and pull-request workflows run only the lightweight smoke unit
tests, not cloud provisioning.

## Local use

Use a dedicated, loopback qtr test server and export the same environment as the
[cloud guest E2E](cloud-vm-e2e.md):

```bash
export QTR_API_URL=http://127.0.0.1:8080/api/v1
export QTR_API_TOKEN_FILE=/path/to/private/api-token
export QTR_IMAGE_ROOT=/path/to/test/images
export QTR_MEDIA_ROOT=/path/to/test/media
export QTR_LOG_ROOT=/path/to/test/logs
export QTR_CLOUD_SERVICE_LOG=/path/to/test/qtr.log  # optional

task e2e:jepsen
```

Prerequisites:

- Linux with Docker, Python 3.11+, `ssh-keygen`, and GNU `timeout`.
- qtr with KVM, working seed generation, and an active `default` libvirt NAT
  network. Both the controller and guests must reach that network.
- Guest Internet access for the adapter's Debian/Ubuntu cloud-init package setup.
- At least 3 GiB available for guest RAM plus headroom for qtr, the host, and a
  controller JVM capped at 512 MiB. Budget at least 4 GiB free RAM in practice.
- Read access to the API token and qtr image root; serial-log collection is
  best-effort when server log ownership differs from the launcher user.

The launcher reuses the cloud test's pinned Ubuntu image, upstream checksum,
cache, and `QTR_CLOUD_IMAGE_URL` mirror override. See its
[image policy](cloud-vm-e2e.md#image-and-diagnostics).

The versioned `clojure:temurin-21-lein-2.13.0` controller runs as the launcher's
UID/GID. Source, token and ephemeral SSH key mounts are read-only; build outputs
and home are temporary. Maven dependencies are cached in `.tmp/jepsen-smoke-m2`;
set `QTR_JEPSEN_M2` to another writable directory. This is separate from the
root-owned cache used by `task jepsen:test`.

Controller networking defaults to `host`. For an isolated qtr/libvirt container,
`QTR_JEPSEN_NETWORK=container:<id-or-name>` joins that container's network
namespace. Expose qtr on the same loopback port for the host launcher and use
matching mounted resource roots. The controller's API endpoint is otherwise the
same as `QTR_API_URL` with `/api/v1` removed. This test is not intended for remote
or production qtr servers.

SSH host-key checking is disabled only for these disposable guests. Each run
uses a new SSH key and passwordless access for the non-root `jepsen` user. The
smoke entry point supplies explicit SSH connection/read bounds to the pinned
SSHJ remote instead of relying on its default transport timeouts.

## Failure handling and diagnostics

`task e2e:jepsen` has a 30-minute deadline and a 120-second cleanup grace period.
The controller also has its own deadline. Individual API calls, SSH transports,
remote commands and readiness polls are bounded.

On normal errors or TERM, the launcher removes only the controller ID recorded
in its private Docker cidfile, then explicitly cleans its two known run IDs.
If run cleanup fails or the controller cannot be removed, it retains the base
for recovery. An uncatchable kill can still bypass the launcher; use the exact
IDs in `resources.json` with the [managed-run cleanup API](managed-runs.md).
There is no prefix-based cleanup and no automatic run expiry.

Artifacts default to `.tmp/e2e/jepsen`, overridden by `QTR_E2E_ARTIFACT_DIR`:

- `result.json` and bounded lifecycle observations in `events.jsonl`;
- `nodes.json`, `boot-before.json`, and `boot-after.json`;
- cleanup journals for the normal and deliberately failed runs;
- controller, cloud-init, and available serial/service logs;
- `diagnostics-errors.json` if optional log copies were unavailable.

CI uploads diagnostics even on failure. The private launcher configuration,
API token and SSH private key are not copied into artifacts. SSH transport debug
logging is disabled to keep controller diagnostics concise.

Run tests without real guests:

```bash
task e2e:jepsen:test  # Python launcher safety tests; no Docker needed
task jepsen:test     # Clojure adapter/smoke tests in the versioned container
```
