(ns qtr.jepsen.smoke
  "Opt-in, real three-node adapter smoke test; not a database correctness test."
  (:require [cheshire.core :as json]
            [clojure.java.io :as io]
            [clojure.string :as str]
            [jepsen.control :as control]
            [jepsen.control.sshj :as sshj]
            [jepsen.nemesis :as nemesis]
            [qtr.jepsen :as qtr]
            [qtr.jepsen.api :as api])
  (:import (java.util.concurrent Semaphore)
           (net.schmizz.sshj SSHClient)
           (net.schmizz.sshj.transport.verification PromiscuousVerifier)))

(def logical-nodes ["n1" "n2" "n3"])
(def ^:dynamic *nano-time* #(System/nanoTime))
(def ^:dynamic *sleep!* #(Thread/sleep %))

(defn require! [condition message]
  (when-not condition
    (throw (ex-info message {:type ::assertion :check message}))))

(defn write-json! [directory filename value]
  (spit (io/file directory filename) (json/generate-string value {:pretty true})))

(defn event! [directory value]
  (spit (io/file directory "events.jsonl")
        (str (json/generate-string value) "\n") :append true))

(defn ipv4? [address]
  (and (string? address)
       (re-matches #"[0-9]{1,3}(?:\.[0-9]{1,3}){3}" address)
       (every? #(<= 0 (Long/parseLong %) 255) (str/split address #"\."))))

(defn boot-id? [value]
  (and (string? value)
       (boolean (re-matches #"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}" value))))

(defn ssh-exec!
  "Use Jepsen's pinned SSHJ command implementation with explicit transport bounds.

  The stock remote does not expose connect/read timeout configuration. Create a
  bounded client, then use the same SSHJRemote and control/exec command path. Host
  key verification is disabled only for these disposable test guests."
  [ssh address script]
  (require! (ipv4? address) "invalid guest IPv4 address")
  (require! (false? (:strict-host-key-checking ssh)) "smoke requires disposable SSH trust")
  (with-open [client (doto (SSHClient.)
                      (.setConnectTimeout 5000)
                      (.setTimeout 10000)
                      (.addHostKeyVerifier (PromiscuousVerifier.)))]
    (.connect client ^String address 22)
    (.authPublickey client ^String (:username ssh)
                    ^"[Ljava.lang.String;" (into-array String [(:private-key-path ssh)]))
    (let [remote (sshj/->SSHJRemote 1 (assoc ssh :host address) client (Semaphore. 1))]
      (control/with-ssh (assoc ssh :password nil)
        (control/with-session address remote
          (control/exec :timeout "30s" :bash :-c script))))))

(defn await!
  "Retries a bounded observation until it returns a truthy result or expires."
  [label timeout-ms observe]
  (let [deadline (+ (*nano-time*) (* 1000000 timeout-ms))]
    (loop []
      (when (>= (*nano-time*) deadline)
        (throw (ex-info (str "timed out: " label) {:type ::timeout :check label})))
      (let [result (try (observe) (catch Exception _ nil))]
        (if result
          result
          (do (*sleep!* 1000) (recur)))))))

(defn guest-ready? [status address]
  (and (= "running" (:domainState status))
       (true? (:guestAgentReady status))
       (true? (:networkInterfacesAvailable status))
       (some #(and (= "ipv4" (:type %)) (true? (:usable %)) (= address (:address %)))
             (mapcat :addresses (:interfaces status)))))

(defn ready-boot! [cluster logical previous]
  (let [{:keys [vm-id address]} (get (qtr/node-map cluster) logical)
        ssh (:ssh (qtr/test-options cluster))]
    (await! (str logical " guest/SSH readiness") 240000
            (fn []
              (let [status (api/get! (:api cluster) ["vms" vm-id "guest-status"] 5000)]
                (when (guest-ready? status address)
                  (let [boot (str/trim (ssh-exec! ssh address "cat /proc/sys/kernel/random/boot_id"))]
                    (when (and (boot-id? boot) (not= previous boot))
                      (ssh-exec! ssh address "sudo -n cloud-init status --wait")
                      (require! (= logical (str/trim (ssh-exec! ssh address "hostname")))
                                "cloud-init hostname mismatch")
                      boot))))))))

(defn fault! [subject directory action targets]
  (let [operation {:type :invoke :f action :value targets}
        result (nemesis/invoke! subject {} operation)
        expected (if (string? targets) [targets] (vec targets))]
    ;; Never persist arbitrary exception messages supplied by SSH or the API.
    (event! directory {:phase "fault" :action action :targets targets
                       :completed (:qtr/logical-nodes result)
                       :ok (not (contains? result :qtr/error))})
    (require! (not (contains? result :qtr/error)) "nemesis reported an error")
    (require! (= expected (:qtr/logical-nodes result)) "nemesis target mismatch")
    result))

(defn wait-state! [cluster logical expected]
  (let [vm-id (get-in (qtr/node-map cluster) [logical :vm-id])]
    (await! (str logical " state " expected) 20000
            #(= expected (:domainState (api/get! (:api cluster) ["vms" vm-id "guest-status"] 5000))))))

(defn diagnostics! [cluster directory]
  (doseq [[logical {:keys [address]}] (qtr/node-map cluster)]
    (try
      (spit (io/file directory (str logical "-cloud-init.log"))
            (ssh-exec! (:ssh (qtr/test-options cluster)) address
                       "sudo -n cat /var/log/cloud-init.log /var/log/cloud-init-output.log"))
      (catch Exception _
        (event! directory {:phase "diagnostics" :node logical :available false})))))

(defn exercise-cluster! [cluster directory]
  (let [nodes (qtr/node-map cluster)
        addresses (mapv #(get-in nodes [% :address]) logical-nodes)
        ssh (:ssh (qtr/test-options cluster))
        subject (nemesis/setup! (qtr/qtr-nemesis cluster) {})]
    (require! (= (set logical-nodes) (set (keys nodes))) "expected exactly three nodes")
    (require! (and (every? ipv4? addresses) (= 3 (count (distinct addresses)))) "guest addresses must be distinct IPv4")
    (require! (= addresses (:nodes (qtr/test-options cluster))) "Jepsen node order mismatch")
    (write-json! directory "nodes.json" nodes)
    (try
      (let [before (into {} (map (fn [node] [node (ready-boot! cluster node nil)]) logical-nodes))]
        (write-json! directory "boot-before.json" before)
        ;; Test every directed pair, not just host-to-guest TCP/22 readiness.
        (doseq [source addresses, destination addresses :when (not= source destination)]
          (ssh-exec! ssh source (str "timeout 5 bash -c 'exec 3<>/dev/tcp/" destination "/22'"))
          (event! directory {:phase "peer-ssh-port" :source source :destination destination :ok true}))
        (fault! subject directory :suspend "n1")
        (wait-state! cluster "n1" "paused")
        (doseq [node ["n2" "n3"]]
          (require! (= (before node) (ready-boot! cluster node nil)) "untargeted node rebooted"))
        (fault! subject directory :resume "n1")
        (require! (= (before "n1") (ready-boot! cluster "n1" nil)) "resume changed boot ID")
        (fault! subject directory :reset ["n2" "n3"])
        (let [after {"n1" (ready-boot! cluster "n1" nil)
                     "n2" (ready-boot! cluster "n2" (before "n2"))
                     "n3" (ready-boot! cluster "n3" (before "n3"))}]
          (require! (= (before "n1") (after "n1")) "reset affected untargeted node")
          (write-json! directory "boot-after.json" after))
        ;; Leave one node paused so teardown must actually heal it.
        (fault! subject directory :suspend "n1")
        (wait-state! cluster "n1" "paused"))
      (finally
        (nemesis/teardown! subject {})
        (diagnostics! cluster directory)))
    (require! (empty? (qtr/teardown-diagnostics subject)) "nemesis teardown failed")
    (ready-boot! cluster "n1" nil)
    (event! directory {:phase "teardown-resume" :node "n1" :ok true})))

(defn verify-cleaned! [qtr-api run-id nodes directory]
  (let [run (api/get! qtr-api ["runs" run-id])
        names (mapv #(qtr/resource-names run-id %) nodes)
        expected (set (mapcat (fn [{:keys [vm-id image-id seed-id]}]
                               [["vm" vm-id] ["image" image-id] ["media" seed-id]]) names))]
    (write-json! directory (str run-id "-cleanup.json") run)
    (require! (= "closed" (:state run)) "run cleanup did not close")
    (require! (= expected (set (map (juxt :kind :id) (:resources run)))) "cleanup resource set mismatch")
    (require! (every? #(= "deleted" (:state %)) (:resources run)) "run retains undeleted resources")
    (doseq [{:keys [vm-id]} names]
      (let [missing? (try (api/get! qtr-api ["vms" vm-id]) false
                          (catch Exception error (if (api/http-error? error 404) true (throw error))))]
        (require! missing? "VM remains after cleanup")))
    (doseq [[path kind] [["images" "image"] ["media" "media"]]
            resource (api/get! qtr-api [path])]
      (require! (not (contains? expected [kind (:id resource)])) "file remains after cleanup"))
    (require! (= run (qtr/cleanup-run! qtr-api run-id)) "repeated cleanup changed a closed run")))

(defn run-smoke! [qtr-api config directory]
  (io/make-parents (io/file directory "events.jsonl"))
  (spit (io/file directory "events.jsonl") "")
  (println "Provisioning three managed Jepsen nodes")
  (event! directory {:phase "provisioning" :runId (:run-id config)})
  (qtr/with-cluster [cluster (qtr/provision! qtr-api config)]
    (println "Verifying SSH, peer connectivity and hypervisor faults")
    (exercise-cluster! cluster directory))
  (verify-cleaned! qtr-api (:run-id config) logical-nodes directory)
  (println "Verifying partial-provisioning rollback")
  (let [failure-id (str (:run-id config) "-fail")
        failed-config (assoc (dissoc config :user-data) :run-id failure-id
                             :user-data-fn (fn [node]
                                             (when (= "n2" node)
                                               (throw (ex-info "intentional failure after first node"
                                                               {:type ::injected})))
                                             (:user-data config)))
        error (try
                (qtr/with-cluster [unexpected (qtr/provision! qtr-api failed-config)]
                  (throw (ex-info "failure injection did not fire" {:type ::assertion})))
                (catch Exception error error))]
    (require! (= ::injected (:type (ex-data error))) "unexpected provisioning failure or failed rollback")
    ;; A non-empty journal proves provisioning actually got past the first VM.
    (verify-cleaned! qtr-api failure-id ["n1"] directory)
    (event! directory {:phase "partial-provisioning-rollback" :runId failure-id :ok true}))
  (write-json! directory "result.json" {:ok true :scope "adapter-smoke" :nodes 3}))

(defn -main [& args]
  (let [directory (atom nil)
        status (try
                 (require! (= 1 (count args)) "expected one private JSON configuration file")
                 (let [input (json/parse-string (slurp (first args)) true)
                       output (:artifactDir input)
                       _ (reset! directory output)
                       qtr-api (qtr/client {:endpoint (:endpoint input)
                                            :token (str/trim (slurp (:tokenFile input)))
                                            :request-timeout-ms 30000})
                       config {:run-id (:runId input) :managed-run? true
                               :nodes logical-nodes :base-image-id (:baseImageId input)
                               :network-id "default" :vcpus 1 :memory-mib 1024
                               :user-data (qtr/debian-cloud-config
                                            {:username "jepsen" :ssh-authorized-key (:publicKey input)})
                               :require-ssh? true :readiness-timeout-ms 420000
                               :ssh {:username "jepsen" :private-key-path (:privateKeyPath input)
                                     :strict-host-key-checking false}}]
                   (run-smoke! qtr-api config output)
                   (println "qtr three-node Jepsen adapter smoke passed")
                   0)
                 (catch Exception error
                   (when @directory
                     (write-json! @directory "result.json"
                                  (merge {:ok false :type (or (:type (ex-data error)) ::unexpected)
                                          :check (:check (ex-data error))
                                          :exception (.getName (class error))}
                                         (select-keys (ex-data error) [:status :method :path]))))
                   (binding [*out* *err*] (println "qtr Jepsen smoke failed; inspect result.json and diagnostics"))
                   1))]
    (shutdown-agents)
    (System/exit status)))
