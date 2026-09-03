(ns qtr.jepsen.cluster
  "Provisioning lifecycle for ephemeral Jepsen nodes on one qtr endpoint."
  (:require [cheshire.core :as json]
            [clojure.string :as str]
            [qtr.jepsen.api :as api])
  (:import (java.net InetSocketAddress Socket)
           (java.util UUID)))

(def ^:private public-id-pattern #"^[A-Za-z0-9][A-Za-z0-9._-]*$")
(def ^:private linux-user-pattern #"^[a-z_][a-z0-9_-]{0,31}$")
(def ^:private max-name-length 63)
(def ^:private max-cleanup-diagnostics 20)
(def ^:private max-diagnostic-chars 512)

(def ^:dynamic *sleep!* (fn [^long milliseconds] (Thread/sleep milliseconds)))
(def ^:dynamic *nano-time* (fn [] (System/nanoTime)))
(def ^:dynamic *tcp-ready?*
  (fn [^String address port timeout-ms]
    (try
      (with-open [socket (Socket.)]
        (.connect socket (InetSocketAddress. address (int port)) (int timeout-ms))
        true)
      (catch Exception _
        false))))

(declare cleanup!)

(defrecord Cluster [api run-id resources nodes logical-order ssh cleanup-lock closed?]
  java.io.Closeable
  (close [this] (cleanup! this)))

(defn- bounded [value]
  (let [value (str value)]
    (if (<= (count value) max-diagnostic-chars)
      value
      (str (subs value 0 max-diagnostic-chars) "..."))))

(defn- valid-public-id? [value max-length]
  (and (string? value)
       (<= 1 (count value) max-length)
       (boolean (re-matches public-id-pattern value))))

(defn- require-public-id! [value label max-length]
  (when-not (valid-public-id? value max-length)
    (throw (ex-info (str label " must contain only letters, numbers, dot, underscore, "
                         "or hyphen and be at most " max-length " characters")
                    {:type ::invalid-config :field label})))
  value)

(defn- require-positive-int! [value label]
  (when-not (pos-int? value)
    (throw (ex-info (str label " must be a positive integer")
                    {:type ::invalid-config :field label})))
  value)

(defn resource-names
  "Returns deterministic qtr resource names for one logical node."
  [run-id logical-node]
  (require-public-id! run-id "run-id" 24)
  (require-public-id! logical-node "logical node" 24)
  (let [stem (str run-id "-" logical-node)
        names {:logical-node logical-node
               :vm-id stem
               :image-id (str stem ".qcow2")
               :seed-id (str stem "-seed.iso")}]
    (doseq [[kind value] (dissoc names :logical-node)]
      (require-public-id! value (name kind) max-name-length))
    names))

(defn- validate-nodes! [nodes]
  (when-not (and (sequential? nodes) (seq nodes))
    (throw (ex-info "nodes must be a non-empty sequence"
                    {:type ::invalid-config :field "nodes"})))
  (doseq [node nodes]
    (require-public-id! node "logical node" 24))
  (when-not (= (count nodes) (count (distinct nodes)))
    (throw (ex-info "logical node names must be unique"
                    {:type ::invalid-config :field "nodes"})))
  (vec nodes))

(defn- validate-config! [config]
  (let [run-id (require-public-id! (:run-id config) "run-id" 24)
        nodes (validate-nodes! (:nodes config))
        base-image-id (require-public-id! (:base-image-id config) "base-image-id" 255)
        network-id (require-public-id! (or (:network-id config) "default") "network-id" 255)
        vcpus (require-positive-int! (or (:vcpus config) 2) "vcpus")
        memory-mib (require-positive-int! (or (:memory-mib config) 2048) "memory-mib")
        readiness-timeout-ms (require-positive-int!
                              (or (:readiness-timeout-ms config) 300000)
                              "readiness-timeout-ms")
        poll-interval-ms (require-positive-int!
                          (or (:poll-interval-ms config) 1000)
                          "poll-interval-ms")
        ssh-connect-timeout-ms (require-positive-int!
                                (or (:ssh-connect-timeout-ms config) 1000)
                                "ssh-connect-timeout-ms")
        ssh (:ssh config)
        ssh-username (:username ssh)
        private-key-path (:private-key-path ssh)
        user-data (:user-data config)
        user-data-fn (:user-data-fn config)]
    (when (and user-data user-data-fn)
      (throw (ex-info "provide either user-data or user-data-fn, not both"
                      {:type ::invalid-config :field "user-data"})))
    (when-not (or (string? user-data) (ifn? user-data-fn))
      (throw (ex-info "user-data or user-data-fn is required"
                      {:type ::invalid-config :field "user-data"})))
    (when-not (and (string? ssh-username) (re-matches linux-user-pattern ssh-username))
      (throw (ex-info "ssh username is invalid"
                      {:type ::invalid-config :field "ssh.username"})))
    (when (str/blank? (str private-key-path))
      (throw (ex-info "ssh private-key-path is required"
                      {:type ::invalid-config :field "ssh.private-key-path"})))
    (doseq [node nodes
            :let [{:keys [image-id]} (resource-names run-id node)]]
      (when (= image-id base-image-id)
        (throw (ex-info "generated clone image would overwrite the base image"
                        {:type ::invalid-config :field "base-image-id"}))))
    (assoc config
           :run-id run-id
           :nodes nodes
           :base-image-id base-image-id
           :network-id network-id
           :vcpus vcpus
           :memory-mib memory-mib
           :readiness-timeout-ms readiness-timeout-ms
           :poll-interval-ms poll-interval-ms
           :ssh-connect-timeout-ms ssh-connect-timeout-ms
           :require-ssh? (if (contains? config :require-ssh?)
                           (boolean (:require-ssh? config))
                           true)
           :ssh {:username ssh-username
                 :private-key-path (str private-key-path)
                 :strict-host-key-checking
                 (if (contains? ssh :strict-host-key-checking)
                   (boolean (:strict-host-key-checking ssh))
                   false)})))

(defn debian-cloud-config
  "Builds conservative cloud-config for a non-root SSH user.

  The public key must be one line. Password authentication is not enabled."
  [{:keys [username ssh-authorized-key]
    :or {username "jepsen"}}]
  (when-not (and (string? username)
                 (not= "root" username)
                 (re-matches linux-user-pattern username))
    (throw (ex-info "cloud-config username is invalid"
                    {:type ::invalid-cloud-config :field "username"})))
  (when-not (and (string? ssh-authorized-key)
                 (not (str/blank? ssh-authorized-key))
                 (not (re-find #"[\r\n]" ssh-authorized-key))
                 (re-matches #"^(ssh-(rsa|ed25519)|ecdsa-[^ ]+) [A-Za-z0-9+/=]+(?: .*)?$"
                             ssh-authorized-key))
    (throw (ex-info "ssh-authorized-key must be one OpenSSH public key"
                    {:type ::invalid-cloud-config :field "ssh-authorized-key"})))
  (let [quoted-key (json/generate-string ssh-authorized-key)]
    (str "#cloud-config\n"
         "users:\n"
         "  - name: " username "\n"
         "    groups: [sudo]\n"
         "    shell: /bin/bash\n"
         "    sudo: [\"ALL=(ALL) NOPASSWD:ALL\"]\n"
         "    ssh_authorized_keys:\n"
         "      - " quoted-key "\n"
         "disable_root: true\n"
         "ssh_pwauth: false\n"
         "package_update: true\n"
         "packages:\n"
         "  - qemu-guest-agent\n"
         "  - openssh-server\n"
         "  - sudo\n"
         "  - iproute2\n"
         "  - iptables\n"
         "  - curl\n"
         "  - ca-certificates\n"
         "  - rsync\n"
         "  - build-essential\n"
         "runcmd:\n"
         "  - [systemctl, enable, --now, qemu-guest-agent.service]\n"
         "  - [systemctl, enable, --now, ssh.service]\n")))

(defn- node-data [config logical-node]
  (let [data (if-let [user-data-fn (:user-data-fn config)]
               (user-data-fn logical-node)
               (:user-data config))]
    (when-not (string? data)
      (throw (ex-info "user-data-fn must return a string"
                      {:type ::invalid-config :field "user-data-fn"})))
    data))

(defn- optional-node-data [config key logical-node]
  (let [value (get config key)]
    (cond
      (nil? value) nil
      (string? value) value
      (ifn? value) (let [result (value logical-node)]
                     (when-not (or (nil? result) (string? result))
                       (throw (ex-info (str (name key) " function must return a string or nil")
                                       {:type ::invalid-config :field (name key)})))
                     result)
      :else (throw (ex-info (str (name key) " must be a string or function")
                            {:type ::invalid-config :field (name key)})))))

(defn- track! [cluster resource]
  (swap! (:resources cluster) conj resource)
  resource)

(defn- usable-ipv4 [status]
  (->> (:interfaces status)
       (mapcat :addresses)
       (filter #(and (= "ipv4" (:type %))
                     (true? (:usable %))
                     (string? (:address %))))
       (map :address)
       sort
       first))

(defn- wait-for-node! [cluster config vm-id]
  (let [timeout-ns (* 1000000 (:readiness-timeout-ms config))
        deadline (+ (*nano-time*) timeout-ns)]
    (loop []
      (let [status (api/get! (:api cluster) ["vms" vm-id "guest-status"])
            address (when (and (true? (:guestAgentReady status))
                               (true? (:networkInterfacesAvailable status)))
                      (usable-ipv4 status))
            ssh-ready? (and address
                            (or (not (:require-ssh? config))
                                (*tcp-ready?* address 22
                                              (:ssh-connect-timeout-ms config))))]
        (if ssh-ready?
          address
          (if (>= (*nano-time*) deadline)
            (throw (ex-info (str "timed out waiting for node " vm-id " readiness")
                            {:type ::readiness-timeout :vm-id vm-id}))
            (do
              (*sleep!* (:poll-interval-ms config))
              (recur))))))))

(defn- provision-node! [cluster config logical-node]
  (let [{:keys [vm-id image-id seed-id] :as names}
        (resource-names (:run-id config) logical-node)
        qtr-api (:api cluster)
        user-data (node-data config logical-node)
        network-config (optional-node-data config :network-config logical-node)
        vendor-data (optional-node-data config :vendor-data logical-node)]
    (api/post! qtr-api ["images" (:base-image-id config) "clone"] {:id image-id})
    (track! cluster {:kind :image :id image-id})

    (api/post! qtr-api ["media" "cloud-init"]
               (cond-> {:id seed-id
                        :instanceId vm-id
                        :localHostname logical-node
                        :userData user-data}
                 (some? network-config) (assoc :networkConfig network-config)
                 (some? vendor-data) (assoc :vendorData vendor-data)))
    (track! cluster {:kind :seed :id seed-id})

    (api/post! qtr-api ["vms"]
               {:name vm-id
                :resources {:vcpus (:vcpus config)
                            :memoryMib (:memory-mib config)}
                :disks [{:imageId image-id
                         :format "qcow2"
                         :bus "virtio-blk"}]
                :networkId (:network-id config)
                :mediaId nil
                :cdroms [{:id "cloud-init" :mediaId seed-id}]
                :console {:graphics "none" :serialLog true}})
    (track! cluster {:kind :vm :id vm-id})

    (api/post! qtr-api ["vms" vm-id "start"] nil)
    (let [address (wait-for-node! cluster config vm-id)
          node (assoc names :address address)]
      (when (some #(= address (:address %)) (vals @(:nodes cluster)))
        (throw (ex-info (str "qtr returned duplicate node address " address)
                        {:type ::duplicate-address :address address})))
      (swap! (:nodes cluster) assoc logical-node node)
      node)))

(defn- expected-cleanup-error? [throwable]
  (or (api/http-error? throwable 404)
      (and (api/http-error? throwable 409)
           (let [detail (str/lower-case
                         (str (get-in (ex-data throwable) [:problem :detail])))]
             (or (str/includes? detail "already inactive")
                 (str/includes? detail "already stopped")
                 (str/includes? detail "already shut off"))))))

(defn- cleanup-call! [cluster resource operation]
  (try
    (operation)
    (swap! (:resources cluster) #(vec (remove #{resource} %)))
    nil
    (catch Throwable throwable
      (if (expected-cleanup-error? throwable)
        (do
          (swap! (:resources cluster) #(vec (remove #{resource} %)))
          nil)
        {:kind (:kind resource)
         :id (:id resource)
         :message (bounded (.getMessage throwable))}))))

(defn cleanup!
  "Deletes only the exact resources recorded by this Cluster.

  Successful cleanup is idempotent. Failed resources remain tracked so callers
  can retry; all cleanup actions are attempted before errors are reported."
  [cluster]
  (locking (:cleanup-lock cluster)
    (if @(:closed? cluster)
      cluster
      (let [snapshot (vec @(:resources cluster))
            by-kind #(reverse (filter (fn [resource] (= % (:kind resource))) snapshot))
            errors (atom [])
            attempt (fn [resource operation]
                      (when-let [error (cleanup-call! cluster resource operation)]
                        (when (< (count @errors) max-cleanup-diagnostics)
                          (swap! errors conj error))))]
        (doseq [resource (by-kind :vm)]
          (let [id (:id resource)]
            (try
              (api/post! (:api cluster) ["vms" id "destroy"] nil)
              (catch Throwable throwable
                (when-not (expected-cleanup-error? throwable)
                  (when (< (count @errors) max-cleanup-diagnostics)
                    (swap! errors conj {:kind :vm :id id :phase :destroy
                                        :message (bounded (.getMessage throwable))})))))
            (attempt resource #(api/delete! (:api cluster) ["vms" id]))))
        (doseq [resource (by-kind :seed)]
          (attempt resource #(api/delete! (:api cluster) ["media" (:id resource)])))
        (doseq [resource (by-kind :image)]
          (attempt resource #(api/delete! (:api cluster) ["images" (:id resource)])))
        (if (seq @errors)
          (throw (ex-info "qtr cluster cleanup failed"
                          {:type ::cleanup-failed :errors @errors}))
          (do
            (reset! (:closed? cluster) true)
            cluster))))))

(defn provision!
  "Creates and waits for an ephemeral qtr cluster before Jepsen run! starts."
  [qtr-api raw-config]
  (let [config (validate-config! raw-config)
        cluster (->Cluster qtr-api (:run-id config) (atom []) (atom {})
                           (:nodes config) (:ssh config) (Object.) (atom false))]
    (try
      (doseq [logical-node (:nodes config)]
        (provision-node! cluster config logical-node))
      cluster
      (catch Throwable original
        (if-let [cleanup-error (try
                                 (cleanup! cluster)
                                 nil
                                 (catch Throwable throwable throwable))]
          (throw (ex-info
                  "qtr provisioning failed and cleanup was incomplete"
                  {:type ::provision-failed
                   :cleanup-errors
                   (or (:errors (ex-data cleanup-error))
                       [{:message (bounded (.getMessage ^Throwable cleanup-error))}])}
                  original))
          (throw original))))))

(defn node-map
  "Returns logical node names mapped to addresses and exact qtr resource IDs."
  [cluster]
  @(:nodes cluster))

(defn test-options
  "Returns the :nodes and :ssh options to merge into a normal Jepsen test map."
  [cluster]
  (let [nodes (node-map cluster)]
    {:nodes (mapv #(get-in nodes [% :address]) (:logical-order cluster))
     :ssh (:ssh cluster)}))

(defmacro with-cluster
  "Binds an already-provisioned Cluster expression and always cleans it up."
  [[binding cluster-expression] & body]
  `(let [~binding ~cluster-expression]
     (try
       ~@body
       (finally
         (cleanup! ~binding)))))

(defn generated-run-id
  "Returns a qtr-safe random run ID for callers that do not need repeatability."
  ([] (generated-run-id "jepsen"))
  ([prefix]
   (require-public-id! prefix "run-id prefix" 12)
   (str prefix "-" (subs (str (UUID/randomUUID)) 0 8))))
