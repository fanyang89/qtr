(ns qtr.jepsen.smoke-test
  (:require [cheshire.core :as json]
            [clojure.java.io :as io]
            [clojure.string :as str]
            [clojure.test :refer :all]
            [qtr.jepsen :as qtr]
            [qtr.jepsen.api :as api]
            [qtr.jepsen.cluster :as cluster]
            [qtr.jepsen.smoke :as smoke])
  (:import (java.util UUID)))

(defn temporary-directory []
  (doto (io/file (System/getProperty "user.home") "tmp" "pi"
                 (str "qtr-smoke-test-" (UUID/randomUUID)))
    .mkdirs))

(defmacro with-directory [[name] & body]
  `(let [~name (temporary-directory)]
     (try ~@body
          (finally (doseq [file# (reverse (file-seq ~name))] (io/delete-file file#))))))

(defn boot-id [generation]
  (format "%08d-0000-4000-8000-000000000001" generation))

(defn fixture [fail-reset?]
  (let [states (atom (zipmap smoke/logical-nodes (repeat "running")))
        boots (atom (zipmap smoke/logical-nodes (repeat 1)))
        calls (atom [])
        nodes (into {} (map-indexed (fn [index node]
                                     [node {:logical-node node :vm-id (str "run-" node)
                                            :address (str "192.0.2." (+ 11 index))}])
                                   smoke/logical-nodes))
        handler (fn [method path]
                  (let [node (last (str/split (second path) #"-"))
                        action (last path)]
                    (swap! calls conj [method path])
                    (if (= :get method)
                      {:domainState (@states node)
                       :guestAgentReady (= "running" (@states node))
                       :networkInterfacesAvailable true
                       :interfaces [{:addresses [{:type "ipv4" :usable true
                                                   :address (get-in nodes [node :address])}]}]}
                      (case action
                        "suspend" (swap! states assoc node "paused")
                        "resume" (swap! states assoc node "running")
                        "reset" (if fail-reset?
                                  (throw (ex-info "injected reset failure" {}))
                                  (swap! boots update node inc))))))
        qtr-api (reify api/QtrApi
                  (request! [_ method path _body] (handler method path))
                  (request! [_ method path _body _timeout] (handler method path)))
        qtr-cluster (cluster/->Cluster qtr-api "run" (atom []) (atom nodes)
                                       smoke/logical-nodes
                                       {:username "jepsen" :private-key-path "/unused"
                                        :strict-host-key-checking false}
                                       (Object.) (atom false))
        ssh (fn [_ssh address script]
              (let [node (first (for [[node entry] nodes :when (= address (:address entry))] node))]
                (cond
                  (str/includes? script "boot_id") (boot-id (@boots node))
                  (= script "hostname") node
                  :else "ok")))]
    {:cluster qtr-cluster :states states :boots boots :calls calls :ssh ssh}))

(deftest validates-observations-and-bounds-polling
  (is (smoke/ipv4? "192.0.2.11"))
  (doseq [address ["256.1.1.1" "127.0.0.1;reboot" "host" nil]]
    (is (not (smoke/ipv4? address))))
  (is (smoke/boot-id? (boot-id 1)))
  (is (not (smoke/boot-id? "not-a-boot-id")))
  (let [clock (atom 0)]
    (binding [smoke/*nano-time* #(swap! clock + 1000000)
              smoke/*sleep!* (constantly nil)]
      (is (thrown-with-msg? clojure.lang.ExceptionInfo #"timed out"
                           (smoke/await! "never-ready" 3 (constantly false)))))))

(deftest real-nemesis-protocol-drives-explicit-faults-and-teardown
  (with-directory [directory]
    (let [{:keys [cluster states boots calls ssh]} (fixture false)]
      (with-redefs [smoke/ssh-exec! ssh]
        (smoke/exercise-cluster! cluster directory))
      (is (every? #(= "running" %) (vals @states)))
      (is (= {"n1" 1 "n2" 2 "n3" 2} @boots))
      (is (= [[:post ["vms" "run-n1" "suspend"]]
              [:post ["vms" "run-n1" "resume"]]
              [:post ["vms" "run-n2" "reset"]]
              [:post ["vms" "run-n3" "reset"]]
              [:post ["vms" "run-n1" "suspend"]]
              [:post ["vms" "run-n1" "resume"]]]
             (filterv #(= :post (first %)) @calls)))
      (let [events (map #(json/parse-string % true)
                        (str/split-lines (slurp (io/file directory "events.jsonl"))))]
        (is (= 6 (count (filter #(= "peer-ssh-port" (:phase %)) events)))))
      (is (.exists (io/file directory "boot-after.json"))))))

(deftest nemesis-errors-are-not-mistaken-for-success
  (with-directory [directory]
    (let [{:keys [cluster states ssh]} (fixture true)]
      (with-redefs [smoke/ssh-exec! ssh]
        (is (thrown-with-msg? clojure.lang.ExceptionInfo #"nemesis reported an error"
                             (smoke/exercise-cluster! cluster directory))))
      (is (= "running" (@states "n1")))
      (is (.exists (io/file directory "n1-cloud-init.log"))))))

(deftest rejects-incomplete-run-cleanup
  (with-directory [directory]
    (let [qtr-api (reify api/QtrApi
                    (request! [_ _method _path _body] {:state "closed" :resources []})
                    (request! [_ _method _path _body _timeout] nil))]
      (is (thrown-with-msg? clojure.lang.ExceptionInfo #"resource set mismatch"
                           (smoke/verify-cleaned! qtr-api "run" ["n1"] directory))))))

(deftest orchestration-requires-the-intentional-partial-failure
  (with-directory [directory]
    (let [cleaned (atom [])
          verified (atom [])]
      (with-redefs [qtr/provision! (fn [_ config]
                                    (is (not (and (:user-data config) (:user-data-fn config))))
                                    (if-let [inject (:user-data-fn config)]
                                      (inject "n2")
                                      :fixture))
                    cluster/cleanup! #(swap! cleaned conj %)
                    smoke/exercise-cluster! (fn [_ _] nil)
                    smoke/verify-cleaned! (fn [_ id nodes _] (swap! verified conj [id nodes]))]
        (smoke/run-smoke! nil {:run-id "run" :user-data "cloud-config"} directory))
      (is (= [:fixture] @cleaned))
      (is (= [["run" ["n1" "n2" "n3"]] ["run-fail" ["n1"]]] @verified))
      (is (true? (:ok (json/parse-string (slurp (io/file directory "result.json")) true)))))))

(deftest reset-recovery-requires-a-new-boot-id
  (let [{:keys [cluster ssh]} (fixture false)
        clock (atom 0)]
    (binding [smoke/*nano-time* #(swap! clock + 100000000000)
              smoke/*sleep!* (constantly nil)]
      (with-redefs [smoke/ssh-exec! ssh]
        (is (thrown-with-msg? clojure.lang.ExceptionInfo #"timed out"
                             (smoke/ready-boot! cluster "n2" (boot-id 1))))))))
