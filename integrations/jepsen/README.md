# qtr Jepsen Adapter

This library provisions ephemeral Jepsen database nodes through one qtr REST
endpoint and provides an optional hypervisor lifecycle nemesis. It does not
replace Jepsen's database, client, workload, checker, operating-system, or
network nemesis implementations.

Jepsen opens control sessions before it runs OS and database setup. Call
`provision!` before `jepsen.core/run!`; do not provision qtr VMs from a Jepsen
`OS/setup!` implementation.

## Prerequisites

- A qtr server reachable from the Jepsen control node over a trusted connection.
  qtr serves plain HTTP by default, so use loopback or a TLS reverse proxy.
- A managed, unattached qcow2 cloud image with cloud-init support.
- A libvirt network reachable from the control node and between all database
  nodes.
- An SSH key pair for the non-root Jepsen user.

Import a Debian cloud image once. qtr validates the uploaded format and never
modifies this base image when the adapter creates linked clones:

```bash
curl --fail-with-body \
  -X PUT \
  -H "Authorization: Bearer $QTR_TOKEN" \
  -H 'Content-Type: application/octet-stream' \
  --data-binary @debian-13-genericcloud-amd64.qcow2 \
  http://127.0.0.1:8080/api/v1/images/debian-base.qcow2
```

## Use from a Jepsen test

Add this project as a checkout dependency or publish it to the repository used
by your Jepsen test. The example below assumes the qtr repository is available
as `checkouts/qtr-jepsen` in the test project's Leiningen checkout path.

```clojure
(ns my-test.core
  (:require [jepsen.core :as jepsen]
            [jepsen.generator :as gen]
            [jepsen.tests :as tests]
            [qtr.jepsen :as qtr]))

(def public-key
  (clojure.string/trim (slurp "/home/jepsen/.ssh/id_ed25519.pub")))

(def api
  (qtr/client {:endpoint "http://127.0.0.1:8080"
               :token (System/getenv "QTR_TOKEN")
               :connect-timeout-ms 5000
               :request-timeout-ms 30000}))

(def cluster-config
  {:run-id "etcd-20260603-01"
   :nodes ["n1" "n2" "n3" "n4" "n5"]
   :base-image-id "debian-base.qcow2"
   :network-id "default"
   :vcpus 2
   :memory-mib 2048
   :user-data (qtr/debian-cloud-config
               {:username "jepsen"
                :ssh-authorized-key public-key})
   :ssh {:username "jepsen"
         :private-key-path "/home/jepsen/.ssh/id_ed25519"
         :strict-host-key-checking false}
   :readiness-timeout-ms 300000
   :require-ssh? true})

(qtr/with-cluster [cluster (qtr/provision! api cluster-config)]
  (let [hypervisor (qtr/qtr-nemesis cluster)
        client-workload (gen/repeat {:type :invoke :f :read})
        fault-schedule (cycle [(gen/sleep 10)
                               {:type :info :f :suspend :value "n1"}
                               (gen/sleep 5)
                               {:type :info :f :resume :value "n1"}
                               (gen/sleep 10)
                               {:type :info :f :reset
                                :value ["n2" "n3"]}])
        test (merge tests/noop-test
                    (qtr/test-options cluster)
                    {:name "my-qtr-test"
                     :nemesis hypervisor
                     :generator (gen/nemesis fault-schedule client-workload)})]
    (jepsen/run! test)))
```

Replace `tests/noop-test`, `client-workload`, and the remaining test-map fields
with the database-specific Jepsen test. `qtr/test-options` uses the discovered
IPv4 addresses as `:nodes` and supplies the configured SSH options. Use
`qtr/node-map` when database setup needs the logical name, IP address, or exact
qtr VM ID:

```clojure
(qtr/node-map cluster)
;; => {"n1" {:logical-node "n1", :address "192.168.122.101",
;;            :vm-id "etcd-20260603-01-n1", ...}}
```

`:user-data`, `:network-config`, and `:vendor-data` may each be a string.
Alternatively, provide `:user-data-fn`, `:network-config`, or `:vendor-data` as
a function from logical node name to a string (or `nil` for the optional
fields). This supports per-node static network configuration without embedding
that policy in qtr.

## Hypervisor nemesis

The adapter accepts only explicit `:reset`, `:reboot`, `:suspend`, and `:resume`
operations. `:value` must be one logical node name, one discovered address, or
a collection of either. It never chooses a random target. Suspended VMs are
best-effort resumed during nemesis teardown.

These are out-of-band hypervisor faults. Continue to use Jepsen's normal SSH
control path and in-guest nemeses for:

- `iptables` network partitions;
- `tc` delay, loss, and packet shaping;
- process kill/pause faults;
- clock skew and file faults.

The adapter deliberately does not implement network partitions because qtr's
current persistent NIC configuration is not a directional partition matrix.

## Cleanup and current scope

`with-cluster` runs cleanup in `finally`. Partial provisioning failures also
attempt rollback. Cleanup destroys and undefines only the exact VM IDs tracked
in memory, then deletes their seed media and linked-clone images. It never
searches by prefix and never deletes the base image. Successful cleanup is
idempotent; failed resources remain tracked for an explicit retry.

Current limitations:

- one qtr endpoint and one libvirt hypervisor per `Cluster`;
- sequential provisioning;
- no persistent qtr-side run lease or ownership labels;
- a hard kill of the Jepsen control process can bypass `finally`, and an
  ambiguous HTTP transport failure can occur after qtr creates a resource but
  before the adapter records success; either case can require manual cleanup of
  the deterministic IDs;
- readiness depends on QEMU Guest Agent network data, and optionally TCP/22;
- no automatic download or checksum verification of base images.

Use a unique, validated `:run-id` for every concurrent test. Resource name
collisions fail instead of overwriting existing resources.

## Development

From the repository root, run:

```bash
task jepsen:test
```

This uses the pinned official Clojure/Leiningen container and does not add
Clojure or Docker to `task check`.
