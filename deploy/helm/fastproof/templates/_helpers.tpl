{{- define "fastproof.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "fastproof.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.labels" -}}
app.kubernetes.io/name: {{ include "fastproof.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: fastproof
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{- end -}}

{{- define "fastproof.selector" -}}
app.kubernetes.io/name: {{ include "fastproof.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: server
{{- end -}}

{{- define "fastproof.image" -}}
{{- $tag := default .Chart.AppVersion .Values.image.tag -}}
{{- if .Values.image.digest -}}
{{- printf "%s:%s@%s" .Values.image.repository $tag .Values.image.digest -}}
{{- else -}}
{{- printf "%s:%s" .Values.image.repository $tag -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "fastproof.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- required "serviceAccount.name is required when serviceAccount.create is false" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.dependencyFullname" -}}
{{- $root := .root -}}
{{- if contains .name $root.Release.Name -}}
{{- $root.Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" $root.Release.Name .name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.bundledDatabase" -}}
{{- if or .Values.postgresql.enabled .Values.cnpg.enabled -}}true{{- end -}}
{{- end -}}

{{- define "fastproof.postgresqlSecretName" -}}
{{- tpl .Values.postgresql.customUser.existingSecret . -}}
{{- end -}}

{{- define "fastproof.valkeySecretName" -}}
{{- tpl .Values.valkey.auth.usersExistingSecret . -}}
{{- end -}}

{{- define "fastproof.garageName" -}}
{{- include "fastproof.dependencyFullname" (dict "root" . "name" "garage") -}}
{{- end -}}

{{- define "fastproof.garageSecretName" -}}
{{- if .Values.garage.credentials.existingSecret -}}
{{- .Values.garage.credentials.existingSecret -}}
{{- else -}}
{{- printf "%s-credentials" (include "fastproof.garageName" .) -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.databaseSecret" -}}
{{- if .Values.postgresql.enabled -}}
{{- toJson (dict "name" (include "fastproof.postgresqlSecretName" .) "key" "database-url") -}}
{{- else if .Values.cnpg.enabled -}}
{{- toJson (dict "name" (printf "%s-app" (include "fastproof.dependencyFullname" (dict "root" . "name" "cnpg"))) "key" "uri") -}}
{{- else if .Values.database.existingSecret -}}
{{- toJson (dict "name" .Values.database.existingSecret "key" .Values.database.secretKey) -}}
{{- else -}}
{{- toJson (dict "name" (printf "%s-database" (include "fastproof.fullname" .)) "key" "database-url") -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.databaseCa" -}}
{{- if or .Values.database.ca.existingSecret .Values.database.ca.existingConfigMap -}}
{{- toJson (dict "secret" .Values.database.ca.existingSecret "configMap" .Values.database.ca.existingConfigMap "key" .Values.database.ca.key) -}}
{{- else if .Values.cnpg.enabled -}}
{{- toJson (dict "secret" (printf "%s-ca" (include "fastproof.dependencyFullname" (dict "root" . "name" "cnpg"))) "configMap" "" "key" "ca.crt") -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.databaseSslMode" -}}
{{- if .Values.database.sslMode -}}
{{- .Values.database.sslMode -}}
{{- else if .Values.cnpg.enabled -}}
verify-full
{{- end -}}
{{- end -}}

{{- define "fastproof.existingBlobBackend" -}}
{{- $cm := lookup "v1" "ConfigMap" .Release.Namespace (include "fastproof.fullname" .) -}}
{{- dig "data" "FASTPROOF_BLOB_BACKEND" "" $cm -}}
{{- end -}}

{{- define "fastproof.blobBackend" -}}
{{- if .Values.blob.backend -}}
{{- .Values.blob.backend -}}
{{- else if .Values.blob.s3.endpoint -}}
s3
{{- else -}}
{{- $existing := include "fastproof.existingBlobBackend" . -}}
{{- if $existing -}}
{{- $existing -}}
{{- else if .Release.IsUpgrade -}}
{{- fail "this is an upgrade but the blob backend of the existing release cannot be read (no cluster access while rendering). The default backend is now s3 with the bundled Garage, which would start with an empty bucket. Set blob.backend explicitly: fs to keep the attachments on the existing volume, or s3 to move to object storage" -}}
{{- else -}}
s3
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.blobBackendKept" -}}
{{- if and (not .Values.blob.backend) (not .Values.blob.s3.endpoint) (eq (include "fastproof.blobBackend" .) "fs") -}}true{{- end -}}
{{- end -}}

{{- define "fastproof.garageEnabled" -}}
{{- if and .Values.garage.enabled (eq (include "fastproof.blobBackend" .) "s3") (not .Values.blob.s3.endpoint) -}}true{{- end -}}
{{- end -}}

{{- define "fastproof.s3CredentialsSecret" -}}
{{- if include "fastproof.garageEnabled" . -}}
{{- toJson (dict "name" (include "fastproof.garageSecretName" .) "accessKey" .Values.garage.credentials.accessKeyKey "secretKey" .Values.garage.credentials.secretKeyKey) -}}
{{- else if .Values.blob.s3.credentials.existingSecret -}}
{{- toJson (dict "name" .Values.blob.s3.credentials.existingSecret "accessKey" .Values.blob.s3.credentials.accessKeyKey "secretKey" .Values.blob.s3.credentials.secretKeyKey) -}}
{{- else -}}
{{- toJson (dict "name" (printf "%s-s3" (include "fastproof.fullname" .)) "accessKey" "access-key" "secretKey" "secret-key") -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.adminTokensEnabled" -}}
{{- $t := .Values.auth.adminTokens -}}
{{- if or $t.existingSecret $t.tokens $t.generate -}}true{{- end -}}
{{- end -}}

{{- define "fastproof.adminTokensSecret" -}}
{{- if .Values.auth.adminTokens.existingSecret -}}
{{- toJson (dict "name" .Values.auth.adminTokens.existingSecret "key" .Values.auth.adminTokens.key) -}}
{{- else -}}
{{- toJson (dict "name" (printf "%s-admin-tokens" (include "fastproof.fullname" .)) "key" "tokens") -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.accessEnabled" -}}
{{- $a := .Values.auth.access -}}
{{- if or $a.existingConfigMap $a.roles $a.bindings -}}true{{- end -}}
{{- end -}}

{{- define "fastproof.oidcSecret" -}}
{{- if .Values.oidc.clientSecret.existingSecret -}}
{{- toJson (dict "name" .Values.oidc.clientSecret.existingSecret "key" .Values.oidc.clientSecret.key) -}}
{{- else -}}
{{- toJson (dict "name" (printf "%s-oidc" (include "fastproof.fullname" .)) "key" "client-secret") -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.oidcRedirectUrl" -}}
{{- if .Values.oidc.redirectUrl -}}
{{- .Values.oidc.redirectUrl -}}
{{- else -}}
{{- printf "%s/auth/callback" (trimSuffix "/" .Values.publicUrl) -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.mediaKey" -}}
{{- $m := .Values.server.mediaUrlKey | default dict -}}
{{- if $m.existingSecret -}}
{{- toJson (dict "name" $m.existingSecret "key" ($m.key | default "media-url-key")) -}}
{{- else -}}
{{- toJson (dict "name" (printf "%s-media-key" (include "fastproof.fullname" .)) "key" "media-url-key") -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.secretsKey" -}}
{{- $k := .Values.integrations.secretsKey -}}
{{- if $k.existingSecret -}}
{{- toJson (dict "name" $k.existingSecret "key" $k.key) -}}
{{- else if $k.value -}}
{{- toJson (dict "name" (printf "%s-integrations" (include "fastproof.fullname" .)) "key" "secrets-key") -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.smtpPassword" -}}
{{- $s := .Values.integrations.smtp -}}
{{- if and $s.url $s.username -}}
{{- if $s.password.existingSecret -}}
{{- toJson (dict "name" $s.password.existingSecret "key" $s.password.key) -}}
{{- else -}}
{{- toJson (dict "name" (printf "%s-integrations" (include "fastproof.fullname" .)) "key" "smtp-password") -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.valkeyUrlSecret" -}}
{{- if .Values.valkey.enabled -}}
{{- toJson (dict "name" (include "fastproof.valkeySecretName" .) "key" "url") -}}
{{- else if .Values.externalValkey.existingSecret -}}
{{- toJson (dict "name" .Values.externalValkey.existingSecret "key" .Values.externalValkey.key) -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.replicasAboveOne" -}}
{{- if or .Values.autoscaling.enabled (gt (int .Values.replicaCount) 1) .Values.roles.split -}}true{{- end -}}
{{- end -}}

{{- define "fastproof.fsPvc" -}}
{{- if and (eq (include "fastproof.blobBackend" .) "fs") .Values.blob.fs.persistence.enabled -}}true{{- end -}}
{{- end -}}

{{- define "fastproof.migrationJob" -}}
{{- if eq .Values.migrations.mode "job" -}}true{{- end -}}
{{- end -}}

{{- define "fastproof.migrateEnv" -}}
{{- if not (include "fastproof.migrationJob" .) -}}
auto
{{- else if and (include "fastproof.bundledDatabase" .) .Release.IsInstall -}}
auto
{{- else -}}
off
{{- end -}}
{{- end -}}

{{- define "fastproof.caSource" -}}
{{- $ca := .ca -}}
{{- if $ca.secret -}}
- secret:
    name: {{ $ca.secret }}
    items:
      - key: {{ $ca.key }}
        path: {{ .path }}
{{- else -}}
- configMap:
    name: {{ $ca.configMap }}
    items:
      - key: {{ $ca.key }}
        path: {{ .path }}
{{- end }}
{{- end -}}

{{- define "fastproof.validate" -}}
{{- include "fastproof.validateStreaming" . -}}
{{- $dbSources := 0 -}}
{{- range $s := list .Values.postgresql.enabled .Values.cnpg.enabled .Values.database.existingSecret .Values.database.url -}}
{{- if $s }}{{ $dbSources = add1 $dbSources }}{{ end -}}
{{- end -}}
{{- if eq (int $dbSources) 0 -}}
{{- fail "FastProof needs PostgreSQL: set postgresql.enabled, cnpg.enabled, database.existingSecret or database.url" -}}
{{- end -}}
{{- if gt (int $dbSources) 1 -}}
{{- fail "set only one of postgresql.enabled, cnpg.enabled, database.existingSecret and database.url" -}}
{{- end -}}
{{- if and .Values.cnpg.enabled (not (.Capabilities.APIVersions.Has "postgresql.cnpg.io/v1")) -}}
{{- fail "cnpg.enabled needs the CloudNativePG operator and its CRDs: install the operator first" -}}
{{- end -}}
{{- if not (has .Values.migrations.mode (list "job" "auto")) -}}
{{- fail "migrations.mode must be job or auto" -}}
{{- end -}}
{{- if not (has (include "fastproof.blobBackend" .) (list "fs" "s3")) -}}
{{- fail "blob.backend must be fs or s3" -}}
{{- end -}}
{{- if and (eq (include "fastproof.blobBackend" .) "s3") (not (include "fastproof.garageEnabled" .)) -}}
{{- if not .Values.blob.s3.endpoint -}}
{{- fail "blob.s3.endpoint is required when blob.backend is s3 and the bundled Garage is off (garage.enabled=false)" -}}
{{- end -}}
{{- if not .Values.blob.s3.bucket -}}
{{- fail "blob.s3.bucket is required with an external S3 endpoint" -}}
{{- end -}}
{{- $c := .Values.blob.s3.credentials -}}
{{- if and (not $c.existingSecret) (not (and $c.accessKey $c.secretKey)) -}}
{{- fail "S3 credentials are required: set blob.s3.credentials.existingSecret (or accessKey and secretKey)" -}}
{{- end -}}
{{- end -}}
{{- if and (include "fastproof.replicasAboveOne" .) (eq (include "fastproof.blobBackend" .) "fs") -}}
{{- if not (has "ReadWriteMany" .Values.blob.fs.persistence.accessModes) -}}
{{- fail "more than one replica (or roles.split) with blob.backend=fs needs a shared volume: set blob.fs.persistence.accessModes to [ReadWriteMany] or use blob.backend=s3" -}}
{{- end -}}
{{- end -}}
{{- if not (or (include "fastproof.adminTokensEnabled" .) .Values.oidc.issuer) -}}
{{- fail "FastProof never runs without authentication: set auth.adminTokens (existingSecret, tokens or generate) or oidc.issuer" -}}
{{- end -}}
{{- if .Values.oidc.issuer -}}
{{- if not .Values.publicUrl -}}
{{- fail "publicUrl is required with OIDC, for example https://fastproof.example.com" -}}
{{- end -}}
{{- if not .Values.oidc.clientId -}}
{{- fail "oidc.clientId is required when oidc.issuer is set" -}}
{{- end -}}
{{- if not (or .Values.oidc.clientSecret.existingSecret .Values.oidc.clientSecret.value) -}}
{{- fail "oidc.clientSecret.existingSecret (or oidc.clientSecret.value) is required when oidc.issuer is set" -}}
{{- end -}}
{{- end -}}
{{- $smtp := .Values.integrations.smtp -}}
{{- if $smtp.url -}}
{{- if not $smtp.from -}}
{{- fail "integrations.smtp.from is required with integrations.smtp.url, for example FastProof <fastproof@example.com>" -}}
{{- end -}}
{{- if and $smtp.username (not (or $smtp.password.existingSecret $smtp.password.value)) -}}
{{- fail "integrations.smtp.password.existingSecret (or value) is required with integrations.smtp.username" -}}
{{- end -}}
{{- if and (not $smtp.username) (or $smtp.password.existingSecret $smtp.password.value) -}}
{{- fail "integrations.smtp.username is required with integrations.smtp.password" -}}
{{- end -}}
{{- end -}}
{{- if and .Values.valkey.enabled .Values.externalValkey.existingSecret -}}
{{- fail "set either valkey.enabled or externalValkey.existingSecret, not both" -}}
{{- end -}}
{{- if and .Values.auth.access.existingConfigMap (or .Values.auth.access.roles .Values.auth.access.bindings) -}}
{{- fail "set either auth.access.existingConfigMap or auth.access.roles/bindings, not both" -}}
{{- end -}}
{{- if and .Values.networkPolicy.enabled .Values.networkPolicy.egress.enabled -}}
{{- $e := .Values.networkPolicy.egress -}}
{{- if and (not (include "fastproof.bundledDatabase" .)) (not $e.database.to) -}}
{{- fail "networkPolicy.egress.database.to is required for an external database while the egress policy is enabled" -}}
{{- end -}}
{{- if and (eq (include "fastproof.blobBackend" .) "s3") (not (include "fastproof.garageEnabled" .)) (not $e.s3.to) -}}
{{- fail "networkPolicy.egress.s3.to is required for an external S3 endpoint while the egress policy is enabled" -}}
{{- end -}}
{{- if and .Values.oidc.issuer (not $e.oidc.to) -}}
{{- fail "networkPolicy.egress.oidc.to is required with OIDC while the egress policy is enabled" -}}
{{- end -}}
{{- if and .Values.externalValkey.existingSecret (not $e.valkey.to) -}}
{{- fail "networkPolicy.egress.valkey.to is required for an external Valkey while the egress policy is enabled" -}}
{{- end -}}
{{- if and .Values.otel.endpoint (not $e.otlp.to) -}}
{{- fail "networkPolicy.egress.otlp.to is required with otel.endpoint while the egress policy is enabled" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.needsCa" -}}
{{- $s3 := and (eq (include "fastproof.blobBackend" .) "s3") (not (include "fastproof.garageEnabled" .)) (or .Values.blob.s3.ca.existingSecret .Values.blob.s3.ca.existingConfigMap) -}}
{{- $oidc := and .Values.oidc.issuer (or .Values.oidc.ca.existingSecret .Values.oidc.ca.existingConfigMap) -}}
{{- $tls := and .Values.tls.existingSecret (or .Values.tls.clientCa.existingSecret .Values.tls.clientCa.existingConfigMap) -}}
{{- $streaming := or (include "fastproof.streamingCas" . | fromJsonArray) -}}
{{- if or (include "fastproof.databaseCa" .) $s3 $oidc $tls $streaming -}}true{{- end -}}
{{- end -}}

{{- define "fastproof.streamingCas" -}}
{{- $out := list -}}
{{- if include "fastproof.kafkaMode" . -}}
{{- with include "fastproof.kafkaCa" . -}}
{{- $out = append $out (dict "ca" (. | fromJson) "path" "kafka.crt") -}}
{{- end -}}
{{- end -}}
{{- if .Values.sinks.clickhouse.enabled -}}
{{- $c := include "fastproof.clickhouseConn" . | fromJson -}}
{{- with $c.ca -}}
{{- $out = append $out (dict "ca" . "path" "clickhouse.crt") -}}
{{- end -}}
{{- end -}}
{{- if .Values.sinks.opensearch.enabled -}}
{{- $c := include "fastproof.opensearchConn" . | fromJson -}}
{{- with $c.ca -}}
{{- $out = append $out (dict "ca" . "path" "opensearch.crt") -}}
{{- end -}}
{{- end -}}
{{- toJson $out -}}
{{- end -}}

{{- define "fastproof.secretsChecksum" -}}
{{- $v := .Values -}}
{{- $in := $v.integrations -}}
{{- $ks := $v.kafka.external.sasl -}}
{{- $parts := list
  $v.database.url
  $v.blob.s3.credentials.accessKey $v.blob.s3.credentials.secretKey
  (toJson $v.auth.adminTokens.tokens) (toString $v.auth.adminTokens.generate)
  $v.oidc.clientSecret.value
  $ks.password.value
  $v.sinks.clickhouse.external.password.value
  $v.sinks.opensearch.external.password.value
  $in.secretsKey.value
  $in.smtp.password.value -}}
{{- toJson $parts | sha256sum -}}
{{- end -}}
