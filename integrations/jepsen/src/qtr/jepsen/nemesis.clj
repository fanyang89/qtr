(ns qtr.jepsen.nemesis
  "Jepsen nemesis for explicit qtr hypervisor lifecycle faults."
  (:require [jepsen.nemesis :as nemesis]
            [qtr.jepsen.api :as api]
            [qtr.jepsen.cluster :as cluster]))

(def ^:private supported-actions #{:reset :reboot :suspend :resume})
(def ^:private max-diagnostic-chars 512)

(defn- bounded [value]
  (let [value (str value)]
    (if (<= (count value) max-diagnostic-chars)
      value
      (str (subs value 0 max-diagnostic-chars) "..."))))

(defn- requested-targets [value]
  (cond
    (or (string? value) (keyword? value)) [value]
    (sequential? value) (vec value)
    (set? value) (vec (sort-by str value))
    :else (throw (ex-info "qtr nemesis :value must explicitly name a node or collection"
                          {:type ::invalid-target}))))

(defn- resolve-targets [qtr-cluster value]
  (let [nodes (cluster/node-map qtr-cluster)
        by-address (into {} (map (juxt :address identity) (vals nodes)))
        requested (requested-targets value)]
    (when (empty? requested)
      (throw (ex-info "qtr nemesis target collection must not be empty"
                      {:type ::invalid-target})))
    (loop [remaining requested
           seen #{}
           resolved []]
      (if-let [target (first remaining)]
        (let [logical (if (keyword? target) (name target) (str target))
              node (or (get nodes logical) (get by-address logical))]
          (when-not node
            (throw (ex-info (str "unknown qtr nemesis target " logical)
                            {:type ::invalid-target :target logical})))
          (if (contains? seen (:logical-node node))
            (recur (rest remaining) seen resolved)
            (recur (rest remaining)
                   (conj seen (:logical-node node))
                   (conj resolved node))))
        resolved))))

(defrecord QtrNemesis [qtr-cluster suspended teardown-errors]
  nemesis/Nemesis
  (setup! [this _test]
    this)

  (invoke! [_ _test operation]
    (try
      (let [action (:f operation)]
        (when-not (contains? supported-actions action)
          (throw (ex-info (str "unsupported qtr nemesis action " action)
                          {:type ::unsupported-action})))
        (let [targets (resolve-targets qtr-cluster (:value operation))]
          (doseq [{:keys [vm-id]} targets]
            (api/post! (:api qtr-cluster) ["vms" vm-id (name action)] nil)
            (case action
              :suspend (swap! suspended conj vm-id)
              :resume (swap! suspended disj vm-id)
              nil))
          (assoc operation
                 :type :info
                 :qtr/logical-nodes (mapv :logical-node targets)
                 :qtr/vms (mapv :vm-id targets))))
      (catch Throwable throwable
        (assoc operation
               :type :info
               :qtr/error (bounded (.getMessage throwable))))))

  (teardown! [_ _test]
    (reset! teardown-errors [])
    (doseq [vm-id (sort @suspended)]
      (try
        (api/post! (:api qtr-cluster) ["vms" vm-id "resume"] nil)
        (swap! suspended disj vm-id)
        (catch Throwable throwable
          (swap! teardown-errors conj
                 {:vm-id vm-id :message (bounded (.getMessage throwable))}))))
    nil))

(defn qtr-nemesis
  "Creates a nemesis for explicit :reset, :reboot, :suspend and :resume ops.

  Operation :value must name logical nodes or discovered addresses. This
  adapter intentionally makes no random target selection."
  [qtr-cluster]
  (->QtrNemesis qtr-cluster (atom #{}) (atom [])))

(defn teardown-diagnostics
  "Returns bounded diagnostics from the last best-effort nemesis teardown."
  [qtr-nemesis]
  @(:teardown-errors qtr-nemesis))
