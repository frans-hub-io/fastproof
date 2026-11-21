{{- define "fastproof.imageRef" -}}
{{- $ref := .repository -}}
{{- if .tag -}}{{- $ref = printf "%s:%s" $ref (toString .tag) -}}{{- end -}}
{{- if .digest -}}{{- $ref = printf "%s@%s" $ref .digest -}}{{- end -}}
{{- $ref -}}
{{- end -}}

{{- define "fastproof.kafkaMode" -}}
{{- if eq .Values.ingest.mode "kafka" -}}true{{- end -}}
{{- end -}}

{{- define "fastproof.kafkaBundled" -}}
{{- if .Values.kafka.bundled.enabled -}}{{- .Values.kafka.bundled.mode -}}{{- end -}}
{{- end -}}

{{- define "fastproof.kafkaName" -}}
{{- include "fastproof.dependencyFullname" (dict "root" . "name" "kafka") -}}
{{- end -}}

{{- define "fastproof.kafkaSimpleSecretName" -}}
{{- if .Values.kafka.bundled.simple.credentials.existingSecret -}}
{{- .Values.kafka.bundled.simple.credentials.existingSecret -}}
{{- else -}}
{{- printf "%s-credentials" (include "fastproof.kafkaName" .) -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.kafkaClusterId" -}}
{{- if .Values.kafka.bundled.simple.clusterId -}}
{{- .Values.kafka.bundled.simple.clusterId -}}
{{- else -}}
{{- printf "%s/%s" .Release.Namespace .Release.Name | sha256sum | trunc 16 | b64enc | trimSuffix "==" | replace "+" "-" | replace "/" "_" -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.kafkaBrokers" -}}
{{- if include "fastproof.kafkaBundled" . -}}
{{- printf "%s:9092" (include "fastproof.kafkaName" .) -}}
{{- else -}}
{{- join "," .Values.kafka.external.brokers -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.kafkaTls" -}}
{{- $mode := include "fastproof.kafkaBundled" . -}}
{{- if eq $mode "simple" -}}
{{- if .Values.kafka.bundled.simple.tls.existingSecret -}}true{{- end -}}
{{- else if eq $mode "chart" -}}
{{- if .Values.kafka.tls.enabled -}}true{{- end -}}
{{- else if .Values.kafka.external.tls.enabled -}}true
{{- end -}}
{{- end -}}

{{- define "fastproof.kafkaCa" -}}
{{- $mode := include "fastproof.kafkaBundled" . -}}
{{- if eq $mode "simple" -}}
{{- with .Values.kafka.bundled.simple.tls.existingSecret -}}
{{- toJson (dict "secret" . "configMap" "" "key" "ca.crt") -}}
{{- end -}}
{{- else if eq $mode "chart" -}}
{{- if .Values.kafka.tls.enabled -}}
{{- $secret := ternary .Values.kafka.tls.existingSecret (printf "%s-tls" (include "fastproof.kafkaName" .)) (eq .Values.kafka.tls.source "existing-secret") -}}
{{- toJson (dict "secret" $secret "configMap" "" "key" (default "ca.crt" .Values.kafka.tls.caFilename)) -}}
{{- end -}}
{{- else if .Values.kafka.external.tls.enabled -}}
{{- $c := .Values.kafka.external.tls.ca -}}
{{- if or $c.existingSecret $c.existingConfigMap -}}
{{- toJson (dict "secret" $c.existingSecret "configMap" $c.existingConfigMap "key" $c.key) -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.kafkaSasl" -}}
{{- $mode := include "fastproof.kafkaBundled" . -}}
{{- if eq $mode "simple" -}}
{{- $s := .Values.kafka.bundled.simple -}}
{{- toJson (dict "mechanism" $s.saslMechanism "username" $s.username "secret" (include "fastproof.kafkaSimpleSecretName" .) "key" $s.credentials.key) -}}
{{- else if eq $mode "chart" -}}
{{- if .Values.kafka.auth.client.enabled -}}
{{- toJson (dict "mechanism" "plain" "username" .Values.kafka.auth.clientUser "secret" (default (include "fastproof.kafkaName" .) .Values.kafka.auth.existingSecret) "key" "client-password") -}}
{{- end -}}
{{- else if .Values.kafka.external.sasl.mechanism -}}
{{- $s := .Values.kafka.external.sasl -}}
{{- $secret := default (printf "%s-kafka" (include "fastproof.fullname" .)) $s.password.existingSecret -}}
{{- $key := ternary $s.password.key "password" (not (empty $s.password.existingSecret)) -}}
{{- toJson (dict "mechanism" $s.mechanism "username" $s.username "secret" $secret "key" $key) -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.createTopics" -}}
{{- if or .Values.kafka.topics.create .Values.kafka.bundled.enabled -}}true{{- else -}}false{{- end -}}
{{- end -}}

{{- define "fastproof.sinks" -}}
{{- $s := list -}}
{{- if .Values.sinks.clickhouse.enabled }}{{ $s = append $s "clickhouse" }}{{ end -}}
{{- if .Values.sinks.opensearch.enabled }}{{ $s = append $s "opensearch" }}{{ end -}}
{{- join "," $s -}}
{{- end -}}

{{- define "fastproof.clickhouseName" -}}
{{- include "fastproof.dependencyFullname" (dict "root" . "name" "clickhouse") -}}
{{- end -}}

{{- define "fastproof.clickhouseSecretName" -}}
{{- if .Values.sinks.clickhouse.bundled.credentials.existingSecret -}}
{{- .Values.sinks.clickhouse.bundled.credentials.existingSecret -}}
{{- else -}}
{{- printf "%s-credentials" (include "fastproof.clickhouseName" .) -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.clickhouseConn" -}}
{{- $c := .Values.sinks.clickhouse -}}
{{- if $c.bundled.enabled -}}
{{- $tls := not (empty $c.bundled.tls.existingSecret) -}}
{{- $url := printf "%s://%s:%s" (ternary "https" "http" $tls) (include "fastproof.clickhouseName" .) (ternary "8443" "8123" $tls) -}}
{{- $ca := "" -}}
{{- if $tls }}{{ $ca = dict "secret" $c.bundled.tls.existingSecret "configMap" "" "key" "ca.crt" }}{{ end -}}
{{- toJson (dict "url" $url "user" $c.bundled.user "secret" (include "fastproof.clickhouseSecretName" .) "key" $c.bundled.credentials.key "ca" $ca) -}}
{{- else -}}
{{- $p := $c.external.password -}}
{{- $ca := "" -}}
{{- if or $c.external.ca.existingSecret $c.external.ca.existingConfigMap }}{{ $ca = dict "secret" $c.external.ca.existingSecret "configMap" $c.external.ca.existingConfigMap "key" $c.external.ca.key }}{{ end -}}
{{- $secret := default (printf "%s-clickhouse" (include "fastproof.fullname" .)) $p.existingSecret -}}
{{- toJson (dict "url" $c.external.url "user" $c.external.user "secret" $secret "key" (ternary $p.key "password" (not (empty $p.existingSecret))) "ca" $ca) -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.opensearchService" -}}
{{- printf "%s-%s" .Values.opensearch.clusterName .Values.opensearch.nodeGroup -}}
{{- end -}}

{{- define "fastproof.opensearchConn" -}}
{{- $o := .Values.sinks.opensearch -}}
{{- if .Values.opensearch.enabled -}}
{{- $url := printf "https://%s:9200" (include "fastproof.opensearchService" .) -}}
{{- toJson (dict "url" $url "user" $o.bundled.user "secret" $o.bundled.credentialsSecret "key" "password" "ca" (dict "secret" $o.bundled.tlsSecret "configMap" "" "key" "ca.crt")) -}}
{{- else -}}
{{- $p := $o.external.password -}}
{{- $ca := "" -}}
{{- if or $o.external.ca.existingSecret $o.external.ca.existingConfigMap }}{{ $ca = dict "secret" $o.external.ca.existingSecret "configMap" $o.external.ca.existingConfigMap "key" $o.external.ca.key }}{{ end -}}
{{- $secret := default (printf "%s-opensearch" (include "fastproof.fullname" .)) $p.existingSecret -}}
{{- toJson (dict "url" $o.external.url "user" $o.external.user "secret" $secret "key" (ternary $p.key "password" (not (empty $p.existingSecret))) "ca" $ca) -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.roleList" -}}
{{- $out := list -}}
{{- $r := .Values.roles -}}
{{- if $r.split -}}
{{- $out = append $out (dict "name" "api" "service" true) -}}
{{- $out = append $out (dict "name" "ingest" "service" false) -}}
{{- if include "fastproof.kafkaMode" . -}}
{{- $out = append $out (dict "name" "writer" "service" false) -}}
{{- end -}}
{{- if include "fastproof.sinks" . -}}
{{- $out = append $out (dict "name" "sink" "service" false) -}}
{{- end -}}
{{- end -}}
{{- toJson $out -}}
{{- end -}}

{{- define "fastproof.workloads" -}}
{{- $fullname := include "fastproof.fullname" . -}}
{{- $out := list -}}
{{- if .Values.roles.split -}}
{{- range (include "fastproof.roleList" . | fromJsonArray) -}}
{{- $cfg := index $.Values.roles .name -}}
{{- $roles := prepend $cfg.extraRoles .name | uniq -}}
{{- $out = append $out (dict "name" (printf "%s-%s" $fullname .name) "role" .name "roles" (join "," $roles) "service" .service "replicaCount" $cfg.replicaCount "resources" (default $.Values.resources $cfg.resources) "startupProbe" (merge (deepCopy $cfg.startupProbe) $.Values.startupProbe) "readinessProbe" (merge (deepCopy $cfg.readinessProbe) $.Values.readinessProbe) "livenessProbe" (merge (deepCopy $cfg.livenessProbe) $.Values.livenessProbe) "autoscaling" $cfg.autoscaling "pdb" $cfg.pdb "podAnnotations" (merge (deepCopy $cfg.podAnnotations) $.Values.podAnnotations) "podLabels" (merge (deepCopy $cfg.podLabels) $.Values.podLabels) "nodeSelector" (default $.Values.nodeSelector $cfg.nodeSelector) "tolerations" (default $.Values.tolerations $cfg.tolerations) "affinity" (default $.Values.affinity $cfg.affinity) "topologySpreadConstraints" (default $.Values.topologySpreadConstraints $cfg.topologySpreadConstraints)) -}}
{{- end -}}
{{- else -}}
{{- $out = append $out (dict "name" $fullname "role" "" "roles" "all" "service" true "replicaCount" .Values.replicaCount "resources" .Values.resources "startupProbe" .Values.startupProbe "readinessProbe" .Values.readinessProbe "livenessProbe" .Values.livenessProbe "autoscaling" .Values.autoscaling "pdb" .Values.pdb "podAnnotations" .Values.podAnnotations "podLabels" .Values.podLabels "nodeSelector" .Values.nodeSelector "tolerations" .Values.tolerations "affinity" .Values.affinity "topologySpreadConstraints" .Values.topologySpreadConstraints) -}}
{{- end -}}
{{- toJson $out -}}
{{- end -}}

{{- define "fastproof.workloadSelector" -}}
{{- include "fastproof.selector" .root }}
{{- with .role }}
fastproof.franshub.io/role: {{ . }}
{{- end }}
{{- end -}}

{{- define "fastproof.validateStreaming" -}}
{{- if not (has .Values.ingest.mode (list "direct" "kafka")) -}}
{{- fail "ingest.mode must be direct or kafka" -}}
{{- end -}}
{{- if not (has .Values.search.backend (list "postgres" "clickhouse" "opensearch")) -}}
{{- fail "search.backend must be postgres, clickhouse or opensearch" -}}
{{- end -}}
{{- $k := .Values.kafka -}}
{{- if not (has $k.bundled.mode (list "simple" "chart")) -}}
{{- fail "kafka.bundled.mode must be simple or chart" -}}
{{- end -}}
{{- if and $k.bundled.enabled (eq $k.bundled.mode "chart") (not $k.enabled) -}}
{{- fail "kafka.bundled.mode=chart deploys the CloudPirates kafka chart: also set kafka.enabled=true" -}}
{{- end -}}
{{- if and $k.enabled (not (and $k.bundled.enabled (eq $k.bundled.mode "chart"))) -}}
{{- fail "kafka.enabled switches on the CloudPirates kafka chart: set kafka.bundled.enabled=true and kafka.bundled.mode=chart with it" -}}
{{- end -}}
{{- if include "fastproof.kafkaMode" . -}}
{{- if and $k.bundled.enabled $k.external.brokers -}}
{{- fail "set either kafka.bundled.enabled or kafka.external.brokers, not both" -}}
{{- end -}}
{{- if not (or $k.bundled.enabled $k.external.brokers) -}}
{{- fail "ingest.mode=kafka needs Kafka: set kafka.external.brokers or kafka.bundled.enabled" -}}
{{- end -}}
{{- else if or $k.bundled.enabled $k.external.brokers -}}
{{- fail "Kafka is only used with ingest.mode=kafka: set it or remove kafka.bundled.enabled and kafka.external.brokers" -}}
{{- end -}}
{{- if not (has $k.external.sasl.mechanism (list "" "plain" "scram-sha-256" "scram-sha-512")) -}}
{{- fail "kafka.external.sasl.mechanism must be empty, plain, scram-sha-256 or scram-sha-512" -}}
{{- end -}}
{{- if and $k.external.brokers $k.external.sasl.mechanism -}}
{{- if not $k.external.sasl.username -}}
{{- fail "kafka.external.sasl.username is required with kafka.external.sasl.mechanism" -}}
{{- end -}}
{{- if not (or $k.external.sasl.password.existingSecret $k.external.sasl.password.value) -}}
{{- fail "kafka.external.sasl.password.existingSecret (or value) is required with kafka.external.sasl.mechanism" -}}
{{- end -}}
{{- end -}}
{{- if not (has $k.bundled.simple.saslMechanism (list "plain" "scram-sha-256" "scram-sha-512")) -}}
{{- fail "kafka.bundled.simple.saslMechanism must be plain, scram-sha-256 or scram-sha-512" -}}
{{- end -}}
{{- $ch := .Values.sinks.clickhouse -}}
{{- if $ch.enabled -}}
{{- if and $ch.bundled.enabled $ch.external.url -}}
{{- fail "set either sinks.clickhouse.bundled.enabled or sinks.clickhouse.external.url, not both" -}}
{{- end -}}
{{- if not (or $ch.bundled.enabled $ch.external.url) -}}
{{- fail "sinks.clickhouse.enabled needs sinks.clickhouse.external.url or sinks.clickhouse.bundled.enabled" -}}
{{- end -}}
{{- if and $ch.external.url (not $ch.bundled.enabled) -}}
{{- if not (regexMatch "^https?://" $ch.external.url) -}}
{{- fail "sinks.clickhouse.external.url must be an http:// or https:// URL of the ClickHouse HTTP interface" -}}
{{- end -}}
{{- if not $ch.external.user -}}
{{- fail "sinks.clickhouse.external.user is required" -}}
{{- end -}}
{{- if not (or $ch.external.password.existingSecret $ch.external.password.value) -}}
{{- fail "sinks.clickhouse.external.password.existingSecret (or value) is required" -}}
{{- end -}}
{{- end -}}
{{- else if $ch.bundled.enabled -}}
{{- fail "sinks.clickhouse.bundled.enabled needs sinks.clickhouse.enabled=true" -}}
{{- end -}}
{{- $os := .Values.sinks.opensearch -}}
{{- if $os.enabled -}}
{{- if and .Values.opensearch.enabled $os.external.url -}}
{{- fail "set either opensearch.enabled or sinks.opensearch.external.url, not both" -}}
{{- end -}}
{{- if not (or .Values.opensearch.enabled $os.external.url) -}}
{{- fail "sinks.opensearch.enabled needs sinks.opensearch.external.url or opensearch.enabled (the bundled OpenSearch chart)" -}}
{{- end -}}
{{- if and $os.external.url (not .Values.opensearch.enabled) -}}
{{- if not (regexMatch "^https?://" $os.external.url) -}}
{{- fail "sinks.opensearch.external.url must be an http:// or https:// URL" -}}
{{- end -}}
{{- if not $os.external.user -}}
{{- fail "sinks.opensearch.external.user is required" -}}
{{- end -}}
{{- if not (or $os.external.password.existingSecret $os.external.password.value) -}}
{{- fail "sinks.opensearch.external.password.existingSecret (or value) is required" -}}
{{- end -}}
{{- end -}}
{{- else if .Values.opensearch.enabled -}}
{{- fail "opensearch.enabled needs sinks.opensearch.enabled=true" -}}
{{- end -}}
{{- if and (eq .Values.search.backend "clickhouse") (not $ch.enabled) -}}
{{- fail "search.backend=clickhouse needs sinks.clickhouse.enabled (the sink fills ClickHouse)" -}}
{{- end -}}
{{- if and (eq .Values.search.backend "opensearch") (not $os.enabled) -}}
{{- fail "search.backend=opensearch needs sinks.opensearch.enabled (the sink fills OpenSearch)" -}}
{{- end -}}
{{- if and .Values.roles.split (or .Values.autoscaling.enabled (gt (int .Values.replicaCount) 1)) -}}
{{- fail "with roles.split, set replicas and autoscaling per role under roles.<role> (replicaCount and autoscaling apply to the single Deployment only)" -}}
{{- end -}}
{{- if and .Values.networkPolicy.enabled .Values.networkPolicy.egress.enabled -}}
{{- $e := .Values.networkPolicy.egress -}}
{{- if and (include "fastproof.kafkaMode" .) (not $k.bundled.enabled) (not $e.kafka.to) -}}
{{- fail "networkPolicy.egress.kafka.to is required for external Kafka while the egress policy is enabled" -}}
{{- end -}}
{{- if and $ch.enabled (not $ch.bundled.enabled) (not $e.clickhouse.to) -}}
{{- fail "networkPolicy.egress.clickhouse.to is required for an external ClickHouse while the egress policy is enabled" -}}
{{- end -}}
{{- if and $os.enabled (not .Values.opensearch.enabled) (not $e.opensearch.to) -}}
{{- fail "networkPolicy.egress.opensearch.to is required for an external OpenSearch while the egress policy is enabled" -}}
{{- end -}}
{{- end -}}
{{- end -}}
