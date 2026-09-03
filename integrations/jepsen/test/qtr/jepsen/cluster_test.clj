(ns qtr.jepsen.cluster-test
  (:require [clojure.string :as str]
            [clojure.test :refer :all]
            [qtr.jepsen.api :as api]
            [qtr.jepsen.cluster :as cluster]))

(defrecord FakeApi [calls handler]
  api/QtrApi
  (request! [_ method path body]
    (let [call {:method method :path (vec path) :body body}]
      (swap! calls conj call)
      (handler call))))

(defn fake-api [handler]
  (->FakeApi (atom []) handler))

(defn ready-status [address]
  {:guestAgentReady true
   :networkInterfacesAvailable true
   :interfaces [{:name "ens3"
                 :addresses [{:type "ipv4"
                              :address address
                              :prefix 24
                              :usable true}]}]})

(defn base-config
  ([] (base-config ["n1" "n2"]))
  ([nodes]
   {:run-id "run-123"
    :nodes nodes
    :base-image-id "debian-base.qcow2"
    :network-id "default"
    :vcpus 2
    :memory-mib 2048
    :user-data "#cloud-config\n"
    :require-ssh? true
    :readiness-timeout-ms 1000
    :poll-interval-ms 1
    :ssh-connect-timeout-ms 10
    :ssh {:username "jepsen"
          :private-key-path "/keys/id_ed25519"
          :strict-host-key-checking false}}))

(deftest validates-and-derives-resource-names-before-api-calls
  (is (= {:logical-node "n1"
          :vm-id "run-123-n1"
          :image-id "run-123-n1.qcow2"
          :seed-id "run-123-n1-seed.iso"}
         (cluster/resource-names "run-123" "n1")))
  (is (thrown? clojure.lang.ExceptionInfo
               (cluster/resource-names "bad/run" "n1")))
  (let [qtr (fake-api (constantly nil))]
    (is (thrown-with-msg? clojure.lang.ExceptionInfo
                          #"unique"
                          (cluster/provision! qtr (base-config ["n1" "n1"]))))
    (is (empty? @(:calls qtr)))))

(deftest builds-debian-cloud-config-without-passwords-or-unsafe-keys
  (let [key "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITestKey jepsen@test"
        config (cluster/debian-cloud-config
                {:username "jepsen" :ssh-authorized-key key})]
    (is (str/starts-with? config "#cloud-config\n"))
    (is (str/includes? config "qemu-guest-agent"))
    (is (str/includes? config "iptables"))
    (is (str/includes? config key))
    (is (str/includes? config "ssh_pwauth: false"))
    (is (not (re-find #"(?i)password:" config))))
  (is (thrown? clojure.lang.ExceptionInfo
               (cluster/debian-cloud-config
                {:username "root"
                 :ssh-authorized-key "ssh-ed25519 AAAATestKey"})))
  (is (thrown? clojure.lang.ExceptionInfo
               (cluster/debian-cloud-config
                {:username "jepsen"
                 :ssh-authorized-key "ssh-ed25519 AAAA\nsecond-key"}))))

(deftest provisions-sequentially-polls-readiness-and-returns-test-options
  (let [status-count (atom {})
        network-calls (atom 0)
        qtr (fake-api
             (fn [{:keys [method path]}]
               (if (and (= method :get) (= "guest-status" (last path)))
                 (let [vm-id (second path)
                       count (get (swap! status-count update vm-id (fnil inc 0)) vm-id)]
                   (if (= 1 count)
                     {:guestAgentReady true
                      :networkInterfacesAvailable true
                      :interfaces []}
                     (ready-status (if (str/ends-with? vm-id "n1")
                                     "192.0.2.11"
                                     "192.0.2.12"))))
                 nil)))
        config (assoc (base-config ["n2" "n1"])
                      :network-config (fn [node]
                                        (swap! network-calls inc)
                                        (str "version: 2 # " node)))]
    (binding [cluster/*sleep!* (constantly nil)
              cluster/*tcp-ready?* (fn [_address port _timeout]
                                     (= 22 port))]
      (let [created (cluster/provision! qtr config)]
        (is (= 2 @network-calls))
        (is (= {"n1" "192.0.2.11" "n2" "192.0.2.12"}
               (into {} (map (juxt key (comp :address val)))
                     (cluster/node-map created))))
        (is (= {:nodes ["192.0.2.12" "192.0.2.11"]
                :ssh {:username "jepsen"
                      :private-key-path "/keys/id_ed25519"
                      :strict-host-key-checking false}}
               (cluster/test-options created)))
        (is (= {:id "run-123-n2.qcow2"}
               (:body (first @(:calls qtr)))))
        (is (= {:id "run-123-n1.qcow2"}
               (:body (nth @(:calls qtr) 6))))
        (let [before (count @(:calls qtr))]
          (cluster/cleanup! created)
          (cluster/cleanup! created)
          (is (= (+ before 8) (count @(:calls qtr)))))))))

(deftest rolls-back-exact-resources-after-partial-failure
  (let [qtr (fake-api
             (fn [{:keys [method path body]}]
               (cond
                 (and (= method :post)
                      (= ["media" "cloud-init"] path)
                      (= "run-123-n2-seed.iso" (:id body)))
                 (throw (ex-info "seed creation failed" {:type ::injected}))

                 (and (= method :get) (= "guest-status" (last path)))
                 (ready-status "192.0.2.11")

                 :else nil)))
        error (binding [cluster/*tcp-ready?* (constantly true)]
                (try
                  (cluster/provision! qtr (assoc (base-config) :require-ssh? false))
                  nil
                  (catch clojure.lang.ExceptionInfo throwable throwable)))
        cleanup-paths (->> @(:calls qtr)
                           (drop-while #(not= ["vms" "run-123-n1" "destroy"]
                                             (:path %)))
                           (mapv :path))]
    (is (= "seed creation failed" (.getMessage error)))
    (is (= [["vms" "run-123-n1" "destroy"]
            ["vms" "run-123-n1"]
            ["media" "run-123-n1-seed.iso"]
            ["images" "run-123-n2.qcow2"]
            ["images" "run-123-n1.qcow2"]]
           cleanup-paths))))

(deftest preserves-original-cause-with-bounded-partial-cleanup-diagnostics
  (let [qtr (fake-api
             (fn [{:keys [method path body]}]
               (cond
                 (and (= method :post)
                      (= ["media" "cloud-init"] path)
                      (= "run-123-n2-seed.iso" (:id body)))
                 (throw (ex-info "seed creation failed" {:userData "must-not-copy"}))

                 (and (= method :delete)
                      (= ["images" "run-123-n2.qcow2"] path))
                 (throw (ex-info "cleanup image failed" {}))

                 (and (= method :get) (= "guest-status" (last path)))
                 (ready-status "192.0.2.11")

                 :else nil)))
        error (binding [cluster/*tcp-ready?* (constantly true)]
                (try
                  (cluster/provision! qtr (assoc (base-config) :require-ssh? false))
                  nil
                  (catch clojure.lang.ExceptionInfo throwable throwable)))]
    (is (= :qtr.jepsen.cluster/provision-failed (:type (ex-data error))))
    (is (= "seed creation failed" (.getMessage (.getCause error))))
    (is (= [{:kind :image
             :id "run-123-n2.qcow2"
             :message "cleanup image failed"}]
           (:cleanup-errors (ex-data error))))
    (is (not (contains? (ex-data error) :userData)))))

(deftest with-cluster-cleans-on-body-failure
  (let [qtr (fake-api (constantly nil))
        resources (atom [{:kind :image :id "run-123-n1.qcow2"}])
        qtr-cluster (cluster/->Cluster qtr "run-123" resources (atom {}) []
                                       {} (Object.) (atom false))]
    (is (thrown-with-msg? clojure.lang.ExceptionInfo
                          #"body failed"
                          (cluster/with-cluster [created qtr-cluster]
                            (is (identical? created qtr-cluster))
                            (throw (ex-info "body failed" {})))))
    (is (empty? @resources))
    (is (= [{:method :delete
             :path ["images" "run-123-n1.qcow2"]
             :body nil}]
           @(:calls qtr)))))

(deftest cleanup-attempts-all-resources-surfaces-errors-and-can-retry
  (let [seed-failures (atom 0)
        qtr (fake-api
             (fn [{:keys [method path]}]
               (when (and (= method :delete)
                          (= ["media" "run-123-n1-seed.iso"] path)
                          (= 1 (swap! seed-failures inc)))
                 (throw (ex-info "temporary media failure" {:type ::injected})))
               nil))
        resources (atom [{:kind :image :id "run-123-n1.qcow2"}
                         {:kind :seed :id "run-123-n1-seed.iso"}
                         {:kind :vm :id "run-123-n1"}])
        qtr-cluster (cluster/->Cluster qtr "run-123" resources (atom {}) []
                                       {} (Object.) (atom false))
        error (try
                (cluster/cleanup! qtr-cluster)
                nil
                (catch clojure.lang.ExceptionInfo throwable throwable))]
    (is (= :qtr.jepsen.cluster/cleanup-failed (:type (ex-data error))))
    (is (= [{:kind :seed
             :id "run-123-n1-seed.iso"
             :message "temporary media failure"}]
           (:errors (ex-data error))))
    (is (= [{:kind :seed :id "run-123-n1-seed.iso"}] @resources))
    (cluster/cleanup! qtr-cluster)
    (is (empty? @resources))
    (let [calls (count @(:calls qtr))]
      (cluster/cleanup! qtr-cluster)
      (is (= calls (count @(:calls qtr)))))))
