# External Storage

qtr exposes external storage as backends and volumes. iSCSI protocol details are handled internally by the storage driver.

## Check the Host

Check host storage prerequisites:

```bash
cargo run -- storage status
```

The Fedora host dependencies include `iscsi-initiator-utils`, which provides `iscsid` and `iscsiadm`. See [Getting started](getting-started.md#host-dependencies) for the complete package list.

## Register a Backend

Register and scan an iSCSI-backed storage service:

```bash
cargo run -- storage add iscsi --name lab-san --address 10.0.0.10
cargo run -- storage scan lab-san
cargo run -- storage volumes lab-san
```

## Manage Volume Connections

Connect or disconnect a discovered volume:

```bash
cargo run -- storage connect lab-san/db-data-01
cargo run -- storage disconnect lab-san/db-data-01
```

Storage state is written to `.qtr/storage.yaml` by default. Use `--config` to choose another state file.
