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

{{- define "fastproof.blobBackend" -}}
{{- if .Values.garage.enabled -}}s3{{- else -}}{{- .Values.blob.backend -}}{{- end -}}
{{- end -}}

{{- define "fastproof.s3CredentialsSecret" -}}
{{- if .Values.garage.enabled -}}
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

{{- define "fastproof.valkeyUrlSecret" -}}
{{- if .Values.valkey.enabled -}}
{{- toJson (dict "name" (include "fastproof.valkeySecretName" .) "key" "url") -}}
{{- else if .Values.externalValkey.existingSecret -}}
{{- toJson (dict "name" .Values.externalValkey.existingSecret "key" .Values.externalValkey.key) -}}
{{- end -}}
{{- end -}}

{{- define "fastproof.replicasAboveOne" -}}
{{- if or .Values.autoscaling.enabled (gt (int .Values.replicaCount) 1) -}}true{{- end -}}
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
{{- if not (has .Values.blob.backend (list "fs" "s3")) -}}
{{- fail "blob.backend must be fs or s3" -}}
{{- end -}}
{{- if .Values.garage.enabled -}}
{{- if eq .Values.blob.backend "fs" -}}
{{- fail "garage.enabled stores attachments in the bundled Garage: set blob.backend to s3" -}}
{{- end -}}
{{- if .Values.blob.s3.endpoint -}}
{{- fail "set either garage.enabled or blob.s3.endpoint, not both" -}}
{{- end -}}
{{- else if eq .Values.blob.backend "s3" -}}
{{- if not .Values.blob.s3.endpoint -}}
{{- fail "blob.s3.endpoint is required when blob.backend is s3 (or enable garage)" -}}
{{- end -}}
{{- if not .Values.blob.s3.bucket -}}
{{- fail "blob.s3.bucket is required when blob.backend is s3" -}}
{{- end -}}
{{- $c := .Values.blob.s3.credentials -}}
{{- if and (not $c.existingSecret) (not (and $c.accessKey $c.secretKey)) -}}
{{- fail "S3 credentials are required: set blob.s3.credentials.existingSecret (or accessKey and secretKey)" -}}
{{- end -}}
{{- end -}}
{{- if and (include "fastproof.replicasAboveOne" .) (eq (include "fastproof.blobBackend" .) "fs") -}}
{{- if not (has "ReadWriteMany" .Values.blob.fs.persistence.accessModes) -}}
{{- fail "more than one replica with blob.backend=fs needs a shared volume: set blob.fs.persistence.accessModes to [ReadWriteMany] or use blob.backend=s3" -}}
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
{{- if and (eq .Values.blob.backend "s3") (not .Values.garage.enabled) (not $e.s3.to) -}}
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
{{- $s3 := and (eq (include "fastproof.blobBackend" .) "s3") (not .Values.garage.enabled) (or .Values.blob.s3.ca.existingSecret .Values.blob.s3.ca.existingConfigMap) -}}
{{- $oidc := and .Values.oidc.issuer (or .Values.oidc.ca.existingSecret .Values.oidc.ca.existingConfigMap) -}}
{{- $tls := and .Values.tls.existingSecret (or .Values.tls.clientCa.existingSecret .Values.tls.clientCa.existingConfigMap) -}}
{{- if or (include "fastproof.databaseCa" .) $s3 $oidc $tls -}}true{{- end -}}
{{- end -}}
