(ns qtr.jepsen.api
  "Small authenticated client for qtr's versioned management API."
  (:require [cheshire.core :as json]
            [clojure.string :as str])
  (:import (java.net URI URLEncoder)
           (java.net.http HttpClient HttpClient$Redirect
                          HttpRequest HttpRequest$BodyPublishers
                          HttpResponse$BodyHandlers)
           (java.nio.charset StandardCharsets)
           (java.time Duration)))

(def ^:private max-diagnostic-chars 1024)

(defprotocol QtrApi
  (request! [api method path-segments body]
            [api method path-segments body timeout-ms]
    "Executes one qtr API request. Path segments are encoded by the client.

    The five-argument form caps the request timeout for deadline-bound calls."))

(defn- bounded-string [value]
  (let [value (str value)]
    (if (<= (count value) max-diagnostic-chars)
      value
      (str (subs value 0 max-diagnostic-chars) "..."))))

(defn path-segment
  "Percent-encodes one path segment."
  [value]
  (-> (URLEncoder/encode (str value) StandardCharsets/UTF_8)
      (str/replace "+" "%20")))

(defn- endpoint-uri [endpoint]
  (let [endpoint (str/replace (str endpoint) #"/+$" "")
        uri (URI/create endpoint)
        scheme (some-> (.getScheme uri) str/lower-case)]
    (when-not (and (#{"http" "https"} scheme)
                   (not (str/blank? (.getRawAuthority uri))))
      (throw (ex-info "qtr endpoint must be an absolute http or https URI"
                      {:type ::invalid-endpoint})))
    (when (or (.getUserInfo uri) (.getQuery uri) (.getFragment uri))
      (throw (ex-info "qtr endpoint must not contain user info, a query, or a fragment"
                      {:type ::invalid-endpoint})))
    endpoint))

(defn- request-uri [endpoint path-segments]
  (URI/create
   (str endpoint "/api/v1/"
        (str/join "/" (map path-segment path-segments)))))

(defn- sensitive-values [token body]
  (->> [token
        (when (map? body) (:userData body))
        (when (map? body) (:vendorData body))
        (when (map? body) (:networkConfig body))]
       (filter string?)
       (remove str/blank?)
       distinct))

(defn- redact [value secrets]
  (if-not (string? value)
    value
    (reduce #(str/replace %1 %2 "[REDACTED]") value secrets)))

(defn- safe-problem [body secrets]
  (if (str/blank? body)
    nil
    (try
      (let [problem (json/parse-string body true)]
        (into {}
              (keep (fn [key]
                      (when-let [value (get problem key)]
                        [key (if (string? value)
                               (bounded-string (redact value secrets))
                               value)])))
              [:type :title :status :detail]))
      (catch Exception _
        {:detail (bounded-string (redact body secrets))}))))

(defn http-error?
  "Returns true when throwable is a non-2xx qtr response, optionally with status."
  ([throwable]
   (= ::http-error (:type (ex-data throwable))))
  ([throwable status]
   (and (http-error? throwable)
        (= status (:status (ex-data throwable))))))

(deftype HttpQtrApi [^String endpoint
                     ^String token
                     ^HttpClient client
                     ^Duration request-timeout]
  Object
  (toString [_] "#<HttpQtrApi>")

  QtrApi
  (request! [this method path-segments body]
    (request! this method path-segments body (.toMillis request-timeout)))
  (request! [_ method path-segments body timeout-ms]
    (when-not (pos-int? timeout-ms)
      (throw (ex-info "qtr request timeout must be positive integer milliseconds"
                      {:type ::invalid-timeout})))
    (let [method-name (str/upper-case (name method))
          body-json (when (some? body) (json/generate-string body))
          publisher (if body-json
                      (HttpRequest$BodyPublishers/ofString body-json)
                      (HttpRequest$BodyPublishers/noBody))
          timeout (Duration/ofMillis (min timeout-ms (.toMillis request-timeout)))
          builder (doto (HttpRequest/newBuilder
                         (request-uri endpoint path-segments))
                    (.timeout timeout)
                    (.header "Accept" "application/json")
                    (.header "Authorization" (str "Bearer " token))
                    (.method method-name publisher))
          builder (if body-json
                    (.header builder "Content-Type" "application/json")
                    builder)
          response (.send client (.build builder)
                          (HttpResponse$BodyHandlers/ofString))
          status (.statusCode response)
          response-body (.body response)
          secrets (sensitive-values token body)]
      (if (<= 200 status 299)
        (when-not (or (= 204 status) (str/blank? response-body))
          (json/parse-string response-body true))
        (throw (ex-info (str "qtr API returned HTTP " status)
                        {:type ::http-error
                         :status status
                         :method method-name
                         :path (vec (map str path-segments))
                         :problem (safe-problem response-body secrets)}))))))

(defn client
  "Creates a qtr API client without exposing its bearer token when printed."
  [{:keys [endpoint token connect-timeout-ms request-timeout-ms]
    :or {connect-timeout-ms 5000
         request-timeout-ms 30000}}]
  (when (or (str/blank? (str token))
            (re-find #"[\r\n]" (str token)))
    (throw (ex-info "qtr bearer token must be non-empty and contain no newlines"
                    {:type ::invalid-token})))
  (when-not (and (pos-int? connect-timeout-ms)
                 (pos-int? request-timeout-ms))
    (throw (ex-info "qtr HTTP timeouts must be positive integer milliseconds"
                    {:type ::invalid-timeout})))
  (let [endpoint (endpoint-uri endpoint)
        http-client (-> (HttpClient/newBuilder)
                        (.connectTimeout (Duration/ofMillis connect-timeout-ms))
                        (.followRedirects HttpClient$Redirect/NEVER)
                        .build)]
    (HttpQtrApi. endpoint (str token) http-client
                 (Duration/ofMillis request-timeout-ms))))

(defn get!
  ([api path-segments]
   (request! api :get path-segments nil))
  ([api path-segments timeout-ms]
   (request! api :get path-segments nil timeout-ms)))

(defn post! [api path-segments body]
  (request! api :post path-segments body))

(defn delete! [api path-segments]
  (request! api :delete path-segments nil))
