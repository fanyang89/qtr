# Server-managed runs

Runs group ephemeral VM resources for explicit, recoverable cleanup. They do not
schedule VMs, expire automatically, or provide tenant isolation: the existing
qtr bearer token authorizes all runs. Use a unique run ID for each concurrent
workload. Reusing an ID means addressing the **same run**, not creating a new one.

## API

All endpoints are authenticated and relative to `/api/v1`:

| Method | Path | Behavior |
| --- | --- | --- |
| `PUT` | `/runs/{id}` | Create an active run, or return the existing run unchanged |
| `GET` | `/runs` | List runs and their resource records |
| `GET` | `/runs/{id}` | Inspect one run |
| `POST` | `/runs/{id}/resources` | Record intent and create/recover one resource |
| `POST` | `/runs/{id}/cleanup` | Forcibly stop owned VMs and remove owned resources |

Run IDs contain 1–63 ASCII letters, numbers, dots, underscores or hyphens and
start with a letter or number. A closed run cannot be reopened. Resource IDs
remain reserved in the run journal, including after cleanup; use fresh IDs for
new runs. Existing, unowned resources cannot be adopted.

Resource creation accepts one of three envelopes. The nested `request` uses
the existing clone, cloud-init, or VM creation schema:

```json
{
  "kind": "imageClone",
  "sourceImageId": "debian-base.qcow2",
  "request": {"id": "test-123-n1.qcow2"}
}
```

```json
{
  "kind": "cloudInit",
  "request": {
    "id": "test-123-n1-seed.iso",
    "instanceId": "test-123-n1",
    "localHostname": "n1",
    "userData": "#cloud-config\n"
  }
}
```

```json
{
  "kind": "vm",
  "request": {
    "name": "test-123-n1",
    "resources": {"vcpus": 2, "memoryMib": 2048},
    "disks": [{"imageId": "test-123-n1.qcow2", "format": "qcow2", "bus": "virtio-blk"}],
    "networkId": "default",
    "cdroms": [{"id": "cloud-init", "mediaId": "test-123-n1-seed.iso"}],
    "console": {"graphics": "none", "serialLog": true}
  }
}
```

VM creation defines but does not start the VM. Start it through the ordinary
`POST /vms/{name}/start` endpoint. The source image is only a clone input: it is
**not** enrolled in the run or deleted by run cleanup. Uploaded base images and
networks remain independently managed.

A resource response contains `kind` (`image`, `media`, or `vm`), `id`, and
`state` (`creating`, `ready`, or `deleted`). Retry the same run, resource ID and
normalized request after a lost response. Changed requests or cross-run ID
collisions return `409`. A `creating` record whose resource was published can be
recovered by retrying: qtr verifies its identity rather than adopting its name.

## Cleanup and recovery

**Cleanup is destructive: it forcibly stops the run's VMs.** It must be requested
explicitly. It transitions the run from `active` to `cleaning` before deleting
anything and rejects new provisioning once cleanup starts. It checks all known
resource identities, then removes VMs, seed media and clones, in dependency
order. Existing install reservations, VM references and backing-image guards
still apply. An error stops cleanup; already-completed deletions remain recorded.
Retry the same cleanup endpoint to continue. Success leaves a `closed` record;
repeated cleanup of a closed run is a no-op.

After a client crash, query the run by its known ID and call cleanup. After a
qtr restart, retain the same state, image and media roots and libvirt connection.
There is no automatic restart of VMs or background deletion of interrupted runs.
HTTP request cancellation does not release operation serialization while its
blocking worker is still running.

The server persists a VM UUID before defining the VM. For files it records an
intent, creates a private hard-link identity pin **before** publishing the file,
and compares device/inode identity during recovery and deletion. The pin keeps
the original inode alive, preventing inode reuse from making a replacement look
owned. A same-name replacement, symlink, or existing file without a matching pin
causes a conflict; qtr never falls back to prefix matching.

A crash before publication may leave an unpublished pin. Provisioning then
returns a conflict; explicit run cleanup releases that pin. If a published file
is missing its pin, or a VM name now has a different UUID, inspect the resources
manually. Do not delete the replacement merely to make the test pass. Correct
the out-of-band change or remove only resources whose identity you have verified,
then retry cleanup. Normal API deletion of an owned file leaves its pin until run
cleanup, so disk space may remain allocated in the meantime.

Do not modify resource roots or libvirt definitions concurrently with recovery
using external tools. qtr serializes its own mutations, not administrator shell
commands or independent libvirt clients.

## Persistent state and security

The journal is `STATE_DIR/runs/runs.sqlite3`, in a mode-`0700` directory with a
mode-`0600` database. SQLite uses WAL and full synchronization. Creation requests
are stored for idempotency, including potentially sensitive cloud-init content;
protect backups accordingly. API responses omit those requests and internal
identity tokens. The server holds an exclusive state lock before startup staging
cleanup; a second qtr server must not share the same state directory.

Host ACL setup skips `.run-pins` subtrees, preserving their server-only access.
File identity pins live in mode-`0700` `.run-pins` directories under the image and
media roots. Do not prune these directories or copy the journal independently of
the resource roots. Closed journal records are retained; this first version has
no TTL, garbage-collection API, multi-host recovery, or live-state migration.

## Jepsen and validation

Enable the adapter with `:managed-run? true`. Legacy clients remain supported;
only creations submitted through `/runs/{id}/resources` are enrolled. After the
control process is lost, call `qtr/cleanup-run!` with the original API client
configuration and run ID. See the [adapter guide](../integrations/jepsen/README.md).

The manual CI cloud-guest test uses managed runs. Locally, set
`QTR_CLOUD_MANAGED_RUN=1` before `task e2e:cloud-vm`; the test imports its base
separately and verifies it is unchanged after server-side run cleanup.
