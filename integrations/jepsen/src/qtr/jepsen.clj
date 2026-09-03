(ns qtr.jepsen
  "Public entry points for provisioning Jepsen nodes with qtr."
  (:require [qtr.jepsen.api :as api]
            [qtr.jepsen.cluster :as cluster]
            [qtr.jepsen.nemesis :as qtr-nemesis]))

(def client api/client)
(def provision! cluster/provision!)
(def cleanup! cluster/cleanup!)
(def node-map cluster/node-map)
(def test-options cluster/test-options)
(def resource-names cluster/resource-names)
(def generated-run-id cluster/generated-run-id)
(def debian-cloud-config cluster/debian-cloud-config)
(def qtr-nemesis qtr-nemesis/qtr-nemesis)
(def teardown-diagnostics qtr-nemesis/teardown-diagnostics)

(defmacro with-cluster
  "Binds a provisioned cluster and always cleans up its exact resources."
  [binding & body]
  `(cluster/with-cluster ~binding ~@body))
