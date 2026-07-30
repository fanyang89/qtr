# Web UI and API

qtr serves its Web UI, REST API, OpenAPI reference, and ticketed VNC WebSocket endpoint from one Rust process.

For local development, follow [Getting started](getting-started.md#run-the-development-servers). Frontend implementation notes and routes are documented in [`web/README.md`](../web/README.md).

## Production-Style Server

Build the Web UI and start the authenticated server manually:

```bash
pnpm -C web build
openssl rand -hex 32 > .qtr-api-token
cargo run -- web --api-token-file .qtr-api-token
```

Open <http://127.0.0.1:8080/access> and store the token for the current browser tab. Management endpoints are under `/api/v1`; `/api/v1/health`, `/api/v1/openapi.json`, and `/docs` are public. VNC connections use short-lived, single-use tickets issued by the authenticated API.

## Install Jobs and Server State

Fedora installations are persistent jobs under `/api/v1/install-jobs`. Requests use `mediaId` and `imageId` instead of host paths. The default roots are `.tmp/iso`, `.tmp/disks`, and `.tmp/logs`; server state and VM manifests are stored under `.qtr/server`. Override them with `--media-root`, `--image-root`, `--log-root`, and `--state-dir`.

SQLite uses WAL mode, queued jobs resume after restart, and jobs that were running are marked `interrupted` without deleting uncertain VM resources. Cancelling an interrupted job acknowledges it and releases its resource reservations.

## Installation Media

The ISOs page supports authenticated, streaming upload and protected deletion. Uploads are limited to 32 GiB by default; override the limit with `--max-iso-upload-bytes`. Existing ISO IDs are never overwritten.

The inventory reports invalid files, install-job reservations, and every live or persistent VM tray reference. Deletion is blocked while any reference remains.

VMs support multiple CD-ROM trays with stable IDs. The VM detail page can insert, replace, or eject managed ISO media while a VM is running, blocked, paused, or shut off. Adding or removing an entire tray requires the VM to be shut off without a managed save image. Eject keeps the virtual drive and removes only its media; removing a tray never deletes its ISO. qtr does not force eject guest-locked media.

## Managed Disks

The Disks page creates, expands, and deletes managed raw and qcow2 images without host shell access. It reports the detected format, virtual capacity, VM attachments, and automated-install reservations. Image IDs must use the extension matching their format. Deletion is blocked while an image is attached or reserved, and expansion only increases capacity and requires every attached VM to be powered off.

Managed images can be attached to or detached from powered-off VMs on the VM detail page. A writable image can be attached to only one VM, and qtr refuses to detach a VM's last disk. Expanding virtual capacity does not resize guest partitions or filesystems.

## Network Security

qtr serves plain HTTP. Keep the default loopback binding or put a TLS reverse proxy in front of qtr on trusted networks. The proxy must support WebSocket upgrades for the VNC console. The server logs a warning when plain HTTP listens on a non-loopback address.

## OpenAPI

The running server exposes Swagger UI at `/docs` and its OpenAPI document at `/api/v1/openapi.json`. The versioned document is committed as [`openapi/qtr-v1.json`](../openapi/qtr-v1.json).

Regenerate it after changing API handlers:

```bash
task openapi:generate
```
