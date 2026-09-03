# GitHub Actions

The repository root contains a setup action for running qtr on an ephemeral
`ubuntu-24.04` GitHub-hosted runner. It installs QEMU and libvirt, verifies a
versioned qtr release archive, starts `qemu:///system`, and leaves the qtr API
running for later workflow steps.

## Basic workflow

Pin the action to a full commit SHA in production workflows. Replace the
placeholder below with a reviewed qtr commit.

```yaml
name: VM tests

on:
  pull_request:
  push:
    branches: [main]

permissions:
  contents: read

jobs:
  vm-tests:
    runs-on: ubuntu-24.04
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@v4

      - name: Start qtr
        id: qtr
        uses: fanyang89/qtr@<full-commit-sha>
        with:
          version: 0.1.0

      - name: Check qtr
        run: |
          qtr vm capabilities --json
          curl --fail --silent --show-error "$QTR_API_URL/health"

      - name: Run tests in a prepared guest
        run: |
          cp ci/base.qcow2 "$QTR_IMAGE_ROOT/ci-node.qcow2"
          qtr vm init \
            --name ci-node \
            --disk "$QTR_IMAGE_ROOT/ci-node.qcow2" \
            --no-cdrom \
            --memory-gib 2 \
            --vcpus 2 \
            --output "$RUNNER_TEMP/ci-node.yaml"
          qtr vm apply --file "$RUNNER_TEMP/ci-node.yaml" --start
          qtr vm exec ci-node --script ci/run-tests.sh --timeout-secs 600
          qtr vm rm ci-node --force-stop
```

The prepared guest image must contain QEMU Guest Agent when `qtr vm exec` is
used. Download external guest images in a preceding step and verify them against
an upstream checksum before copying them into `QTR_IMAGE_ROOT`.

## Inputs

| Input | Default | Description |
| --- | --- | --- |
| `version` | required | Exact qtr release version, such as `0.1.0` |
| `repository` | `fanyang89/qtr` | Repository containing release assets |
| `require-kvm` | `true` | Fail unless `/dev/kvm` is readable and writable |
| `api-port` | `8080` | Loopback port for the qtr API |
| `archive-url` | release URL | HTTPS or local-file archive override |
| `checksum-url` | archive URL plus `.sha256` | SHA-256 sidecar override |

`archive-url` and `checksum-url` are intended for repository CI and controlled
artifact mirrors. Both overrides must use HTTPS, except that `file://` is
accepted for local artifacts on the runner.

Setting `require-kvm` to `false` only permits qtr API and non-KVM operations to
start. It does not convert KVM VM definitions to QEMU TCG emulation.

## Outputs and environment

The action exposes these outputs:

- `qtr-path` and `qtr-version`
- `api-url` and `api-token-file`
- `state-dir`, `image-root`, `media-root`, and `log-root`
- `service-log`, `kvm-available`, and `libvirt-uri`

Later steps also receive these environment variables:

- `QTR_API_URL`
- `QTR_API_TOKEN_FILE`
- `QTR_STATE_DIR`
- `QTR_IMAGE_ROOT`
- `QTR_MEDIA_ROOT`
- `QTR_LOG_ROOT`

Read the API token only when an authenticated request is needed:

```bash
token=$(<"$QTR_API_TOKEN_FILE")
curl \
  --fail \
  --header "Authorization: Bearer $token" \
  "$QTR_API_URL/vms"
```

The token is masked in the Actions log and its file is created with mode `0600`.
The API listens only on `127.0.0.1`.

## Runner behavior

The first action version intentionally supports only x86-64
`ubuntu-24.04` GitHub-hosted runners. Setup fails closed when the operating
system, architecture, passwordless `sudo`, libvirt connection, release
checksum, or required KVM access is unavailable.

The action applies GitHub's published `/dev/kvm` udev rule, which makes the KVM
device available to processes in the job. The runner is ephemeral; do not use
this action on a persistent self-hosted machine. A post step stops the qtr API
and removes its temporary state after user steps finish.

Arbitrary nested QEMU guests are not a contractual GitHub-hosted-runner
capability. Keep the runtime probe enabled and use a suitable self-hosted or
larger runner if the workload requires stronger availability guarantees.

## Release assets

A tag such as `v0.1.0` triggers `.github/workflows/release.yml`. The workflow
builds and publishes:

```text
qtr-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
qtr-v0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

The archive contains the qtr executable and built Web UI. The release workflow
also publishes a GitHub artifact attestation for the archive. The setup action
accepts exact versions only and verifies the SHA-256 sidecar before extracting
or executing the artifact.
