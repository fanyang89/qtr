(ns qtr.jepsen.nemesis-test
  (:require [clojure.test :refer :all]
            [jepsen.nemesis :as nemesis]
            [qtr.jepsen.api :as api]
            [qtr.jepsen.cluster :as cluster]
            [qtr.jepsen.nemesis :as qtr-nemesis]))

(defrecord FakeApi [calls handler]
  api/QtrApi
  (request! [_ method path body]
    (let [call {:method method :path (vec path) :body body}]
      (swap! calls conj call)
      (handler call))))

(defn test-cluster [qtr]
  (cluster/->Cluster
   qtr
   "run-123"
   (atom [])
   (atom {"n1" {:logical-node "n1" :vm-id "run-123-n1" :address "192.0.2.11"}
          "n2" {:logical-node "n2" :vm-id "run-123-n2" :address "192.0.2.12"}})
   ["n1" "n2"]
   {}
   (Object.)
   (atom false)))

(deftest invokes-explicit-logical-and-address-targets
  (let [qtr (->FakeApi (atom []) (constantly nil))
        subject (qtr-nemesis/qtr-nemesis (test-cluster qtr))
        operation {:type :invoke :f :suspend :value ["n1" "192.0.2.12" "n1"]
                   :custom "preserved"}
        result (nemesis/invoke! subject {} operation)]
    (is (= :info (:type result)))
    (is (= "preserved" (:custom result)))
    (is (= (:value operation) (:value result)))
    (is (= ["n1" "n2"] (:qtr/logical-nodes result)))
    (is (= [["vms" "run-123-n1" "suspend"]
            ["vms" "run-123-n2" "suspend"]]
           (mapv :path @(:calls qtr))))
    (nemesis/teardown! subject {})
    (is (= [["vms" "run-123-n1" "resume"]
            ["vms" "run-123-n2" "resume"]]
           (mapv :path (drop 2 @(:calls qtr)))))))

(deftest rejects-ambiguous-and-unsupported-operations-without-random-targets
  (let [qtr (->FakeApi (atom []) (constantly nil))
        subject (qtr-nemesis/qtr-nemesis (test-cluster qtr))]
    (doseq [operation [{:type :invoke :f :reset :value nil}
                       {:type :invoke :f :reset :value []}
                       {:type :invoke :f :reset :value "missing"}
                       {:type :invoke :f :partition :value "n1"}]]
      (let [result (nemesis/invoke! subject {} operation)]
        (is (= :info (:type result)))
        (is (string? (:qtr/error result)))
        (is (= (:value operation) (:value result)))))
    (is (empty? @(:calls qtr)))))

(deftest tracks-suspends-and-reports-bounded-teardown-errors
  (let [qtr (->FakeApi
             (atom [])
             (fn [{:keys [path]}]
               (when (= ["vms" "run-123-n2" "resume"] path)
                 (throw (ex-info (apply str (repeat 1000 "x")) {:secret "not-returned"})))
               nil))
        subject (qtr-nemesis/qtr-nemesis (test-cluster qtr))]
    (nemesis/invoke! subject {} {:type :invoke :f :suspend :value ["n1" "n2"]})
    (nemesis/invoke! subject {} {:type :invoke :f :resume :value "n1"})
    (nemesis/teardown! subject {})
    (let [diagnostics (qtr-nemesis/teardown-diagnostics subject)]
      (is (= "run-123-n2" (:vm-id (first diagnostics))))
      (is (< (count (:message (first diagnostics))) 520))
      (is (nil? (:secret (first diagnostics)))))))
