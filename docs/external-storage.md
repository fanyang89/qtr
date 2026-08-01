# External Storage

qtr exposes external storage as backends and volumes. iSCSI protocol details are handled internally by the storage driver.

The current driver manages host discovery and login state only. It does not
publish storage-provider Volumes, persist VM-to-Volume attachments, or update a
VM definition automatically.

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

## Planned Zettide Backend

The planned Zettide backend makes a stable Zettide Volume ID the storage source
of truth instead of exposing an operator-selected `/dev/...` path in the VM
definition. Its managed lifecycle is:

1. Persist attachment intent, the stable operation ID, and the desired host before any external side effect.
2. Request an idempotent publication for the Volume and qtr host.
3. Receive the publication identity, access generation, target portal, IQN, and LUN.
4. Discover and log in through the host iSCSI initiator.
5. Wait for and validate the stable Linux device identity.
6. Attach the device to the intended libvirt disk.
7. Reconcile publication, session, device, and libvirt attachment from the persisted intent after restart.
8. On detach, persist the detaching intent before removing the libvirt attachment, session, and publication in that order.

The backend must treat every step as retryable. A lost response cannot imply
that publication or login failed, and a stale local device path cannot replace
the Volume and publication identities.

The first completed backend will target one Zettide storage node and standard
iSCSI. Distributed Zettide later reuses this contract to fence an old storage
primary and republish the same Volume. qtr may attach that Volume on a caller-
selected replacement host, but qtr does not automatically select or restart the
VM as part of storage failover.

This section is a development target. The only implemented workflow is the
manual iSCSI command sequence above.
