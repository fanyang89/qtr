(defproject qtr-jepsen "0.1.0-SNAPSHOT"
  :description "Jepsen provisioning and hypervisor nemesis adapter for qtr"
  :url "https://github.com/fanyang89/qtr"
  :license {:name "MIT"}
  :dependencies [[org.clojure/clojure "1.12.4"]
                 [jepsen "0.3.11"]
                 [cheshire "6.2.0"]]
  :source-paths ["src"]
  :test-paths ["test"]
  :jvm-opts ["-Dclojure.main.report=stderr"])
