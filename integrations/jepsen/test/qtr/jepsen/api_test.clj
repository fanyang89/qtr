(ns qtr.jepsen.api-test
  (:require [cheshire.core :as json]
            [clojure.string :as str]
            [clojure.test :refer :all]
            [qtr.jepsen.api :as api])
  (:import (com.sun.net.httpserver HttpHandler HttpServer)
           (java.net InetSocketAddress)
           (java.nio.charset StandardCharsets)))

(deftest percent-encodes-whole-path-segments
  (is (= "node%2Fone%20two" (api/path-segment "node/one two"))))

(deftest validates-client-configuration-without-exposing-token
  (is (thrown-with-msg? clojure.lang.ExceptionInfo
                        #"http or https"
                        (api/client {:endpoint "file:///tmp/qtr" :token "token"})))
  (is (thrown-with-msg? clojure.lang.ExceptionInfo
                        #"bearer token"
                        (api/client {:endpoint "http://localhost" :token ""})))
  (let [client (api/client {:endpoint "http://127.0.0.1:1"
                            :token "never-print-this"})]
    (is (not (str/includes? (str client) "never-print-this")))))

(deftest parses-success-json-and-accepts-no-content
  (let [server (HttpServer/create (InetSocketAddress. "127.0.0.1" 0) 0)]
    (.createContext
     server "/"
     (reify HttpHandler
       (handle [_ exchange]
         (if (= "/api/v1/no-content" (.. exchange getRequestURI getPath))
           (.sendResponseHeaders exchange 204 -1)
           (let [bytes (.getBytes (json/generate-string {:ok true})
                                  StandardCharsets/UTF_8)]
             (.sendResponseHeaders exchange 201 (alength bytes))
             (with-open [stream (.getResponseBody exchange)]
               (.write stream bytes)))))))
    (.start server)
    (try
      (let [client (api/client
                    {:endpoint (str "http://127.0.0.1:" (.getPort (.getAddress server)))
                     :token "token"})]
        (is (= {:ok true} (api/post! client ["json"] {:request true})))
        (is (nil? (api/post! client ["no-content"] nil))))
      (finally
        (.stop server 0)))))

(deftest sends-authenticated-json-and-redacts-error-diagnostics
  (let [observed (atom nil)
        token "test-bearer-secret"
        user-data "#cloud-config\nwrite_files: secret-value"
        server (HttpServer/create (InetSocketAddress. "127.0.0.1" 0) 0)]
    (.createContext
     server "/"
     (reify HttpHandler
       (handle [_ exchange]
         (reset! observed
                 {:method (.getRequestMethod exchange)
                  :path (.. exchange getRequestURI getRawPath)
                  :authorization (.getFirst (.getRequestHeaders exchange)
                                            "Authorization")})
         (let [response (json/generate-string
                         {:title "Bad Request"
                          :status 400
                          :detail (str token " " user-data)})
               bytes (.getBytes response StandardCharsets/UTF_8)]
           (.add (.getResponseHeaders exchange) "Content-Type" "application/problem+json")
           (.sendResponseHeaders exchange 400 (alength bytes))
           (with-open [stream (.getResponseBody exchange)]
             (.write stream bytes))))))
    (.start server)
    (try
      (let [port (.getPort (.getAddress server))
            client (api/client {:endpoint (str "http://127.0.0.1:" port)
                                :token token})
            error (try
                    (api/request! client :post ["vms" "node/one two" "status"]
                                  {:userData user-data})
                    nil
                    (catch clojure.lang.ExceptionInfo throwable throwable))]
        (is (api/http-error? error 400))
        (is (= {:method "POST"
                :path "/api/v1/vms/node%2Fone%20two/status"
                :authorization (str "Bearer " token)}
               @observed))
        (let [diagnostic (pr-str (ex-data error))]
          (is (not (str/includes? diagnostic token)))
          (is (not (str/includes? diagnostic user-data)))
          (is (str/includes? diagnostic "[REDACTED]"))))
      (finally
        (.stop server 0)))))
