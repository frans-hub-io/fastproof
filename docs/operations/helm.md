# Helm chart

The chart lives in the repository at `deploy/helm/fastproof`. It installs FastProof with its own
ServiceAccount, the `RuntimeDefault` seccomp profile, a read-only root filesystem, all capabilities dropped,
resource requests and limits, startup, readiness and liveness probes on the operations port, and a
`values.schema.json` that rejects unknown keys and wrong types. It works under restrictive admission
policies (Pod Security `restricted`, Kyverno best-practice policies).

```sh
git clone https://github.com/frans-hub-io/fastproof.git
cd fastproof
helm repo add cnpg https://cloudnative-pg.github.io/charts
helm repo add valkey https://valkey.io/valkey-helm/
helm repo add opensearch https://opensearch-project.github.io/helm-charts
helm dependency build deploy/helm/fastproof
```

Kubernetes 1.30 or newer is required.

## Quick install

PostgreSQL from the chart, attachments on a volume, and a generated bootstrap admin token:

```sh
helm upgrade --install fastproof deploy/helm/fastproof -n qa --create-namespace \
  --set postgresql.enabled=true \
  --set auth.adminTokens.generate=true

kubectl -n qa get secret fastproof-admin-tokens -o jsonpath='{.data.bootstrap-token}' | base64 -d
kubectl -n qa port-forward svc/fastproof 8080:8080
```

The secret holds the token in `bootstrap-token` and only its hash in the tokens file the server reads. Create
managed tokens, then switch `generate` off or replace the token file.

## Choosing the backends

### PostgreSQL

Exactly one of these:

| Option | Values | When |
|---|---|---|
| External database | `database.existingSecret` (key `database-url`) or `database.url` | Managed PostgreSQL or an existing cluster. Recommended for production |
| CloudPirates `postgres` chart 0.22.0 | `postgresql.enabled: true` | A single PostgreSQL 18.7 instance for evaluation and small teams. The chart creates `<release>-fastproof-db` with a random password and the URL |
| CloudNativePG `cluster` chart 0.9.0 | `cnpg.enabled: true` | HA with failover, backups and rolling minor upgrades. Needs the CloudNativePG operator (chart 0.29.1, operator 1.30.1) installed first. FastProof connects with `sslmode=verify-full` using the cluster's CA |

With an external database, give the CA with `database.ca.existingSecret` or `database.ca.existingConfigMap`
and put `sslmode=verify-full` in the URL (or set `database.sslMode`).

### Attachments

| Option | Values | When |
|---|---|---|
| Volume | `blob.backend: fs` (default), `blob.fs.persistence.*` | One replica, or several with a ReadWriteMany volume. The PVC is kept when the release is uninstalled |
| External S3 | `blob.backend: s3`, `blob.s3.endpoint`, `bucket`, `region`, `credentials.existingSecret` (keys `access-key`, `secret-key`), optional `ca` | Production: AWS S3 or any S3-compatible service with path-style addressing |
| Bundled Garage v2.5.0 | `garage.enabled: true`, `blob.backend: s3` | Evaluation and small installations: one Garage node with a generated key and a default bucket |

Garage is AGPL-3.0 licensed and runs as a separate, unmodified service. The chart ships its own small Garage
StatefulSet (non-root, read-only root filesystem, seccomp, probes on `/health`, resources) instead of
Garage's in-repository chart, because single-node setup with a default bucket and a hardened pod need
settings that chart does not expose.

### Valkey

Optional. Without it, replicas exchange live updates through PostgreSQL `LISTEN/NOTIFY`. Use
`valkey.enabled: true` for the official Valkey chart 0.12.1 (image 9.1.4, password generated into
`<release>-fastproof-valkey`), or `externalValkey.existingSecret` with a `url` key.

### Kafka, ClickHouse and OpenSearch

Optional, for large installations; see [Scaling and big data](scaling.md) for when they pay off.

| Values | What it does |
|---|---|
| `ingest.mode: kafka` | Uploads and live logs go through Kafka |
| `kafka.external.*` | Your Kafka: `brokers`, `tls.enabled`, `tls.ca`, `sasl.mechanism` (`plain`, `scram-sha-256`, `scram-sha-512`), `sasl.username`, `sasl.password.existingSecret` |
| `kafka.enabled: true` | The bundled Kafka: the CloudPirates `kafka` chart 0.3.0 (`apache/kafka` 4.4.0 pinned by digest, KRaft, values under `kafka.*`). One broker with its own ServiceAccount (no token), non-root, read-only root filesystem, probes, resources and a 10Gi volume. FastProof connects with SASL PLAIN using the password the chart generates, and creates its topics. See [Bundled Kafka](#bundled-kafka) |
| `sinks.clickhouse.enabled` with `sinks.clickhouse.external.*` | Your ClickHouse: `url` (HTTP interface), `user`, `password`, `ca` |
| `sinks.clickhouse.bundled.enabled: true` | A single-node ClickHouse 26.8 LTS (distroless image) from this chart: non-root, read-only root filesystem, probes on `/ping`, memory caps for about 2 GiB, a `fastproof` user limited to its database, native/MySQL/PostgreSQL ports closed, optional HTTPS from `sinks.clickhouse.bundled.tls.existingSecret` |
| `sinks.opensearch.enabled` with `sinks.opensearch.external.*` | Your OpenSearch 2.19 or 3.x |
| `sinks.opensearch.enabled` with `opensearch.enabled: true` | The official OpenSearch chart 3.9.0 (values under `opensearch.*`), hardened: one node, no root init container, liveness probe, heap from `opensearch.opensearchJavaOpts`, security plugin with generated TLS and a `fastproof` user limited to `fastproof-*` |
| `search.backend` | `postgres`, `clickhouse` or `opensearch`; needs the matching sink |
| `roles.split: true` | Separate Deployments per role; see [Several replicas](#several-replicas) |

Set either `kafka.enabled` or `kafka.external.brokers`; the chart fails with a clear message when both are set,
when `ingest.mode: kafka` has neither, or when Kafka is configured with `ingest.mode: direct`.

The bundled OpenSearch keeps the official chart's fixed names (`fastproof-opensearch-node`, Secrets
`fastproof-opensearch-tls`, `fastproof-opensearch-security`, `fastproof-opensearch-credentials`), so install
one per namespace or change `opensearch.clusterName`, `opensearch.masterService`, `opensearch.secretMounts`,
`opensearch.securityConfig.config.securityConfigSecret` and `sinks.opensearch.bundled.*` together. The security
configuration is loaded into the cluster on first start only; change users later with `securityadmin.sh` or
the security REST API. Set `sinks.opensearch.bundled.generateTls: false` to bring your own certificate (for
example from cert-manager with `privateKey.encoding: PKCS8`) in `fastproof-opensearch-tls` with `ca.crt`,
`tls.crt` and `tls.key` and the subject `CN=fastproof-opensearch-node`.

#### Bundled Kafka

`kafka.enabled: true` with `ingest.mode: kafka` is all it takes: the chart deploys the broker and points
FastProof at the `<release>-kafka` Service. FastProof's own settings (`kafka.topics.*`, `kafka.consumer.*`,
`kafka.clientId` and the rest) sit next to the broker's.

| Values | Default | Meaning |
|---|---|---|
| `kafka.auth.clientUser` | `fastproof` | SASL PLAIN user FastProof connects as. The chart supports PLAIN only (no SCRAM) |
| `kafka.auth.existingSecret` | none | Secret with `client-password`, `inter-broker-password` and `controller-password`. Without it the chart generates `<release>-kafka` once and keeps it on upgrades |
| `kafka.tls.enabled`, `kafka.tls.source` | `false`, `self-signed` | TLS on every listener. `self-signed` generates a CA and a certificate once, `cert-manager` requests one from `kafka.tls.certManager.issuerRef`, `existing-secret` uses `kafka.tls.existingSecret` |
| `kafka.tls.existingSecret`, `kafka.tls.caFilename` | none, `ca.crt` | Your certificate Secret (`tls.crt`, `tls.key` and the CA); FastProof trusts the CA from it |
| `kafka.serviceAccount.create` | `true` | Dedicated ServiceAccount; `automountServiceAccountToken` is `false` |
| `kafka.replicaCount` | `1` | Brokers. Replication factors follow the broker count, and a PodDisruptionBudget is added from two brokers |
| `kafka.persistence.enabled`, `kafka.persistence.size` | `true`, `10Gi` | Broker volume |
| `kafka.heapOpts`, `kafka.resources` | `-Xmx512m -Xms512m`, requests `100m`/`768Mi`, limits `1`/`1Gi` | Keep the memory limit about twice the heap |
| `kafka.kafkaConfig.autoCreateTopicsEnable` | `false` | FastProof creates its topics with `kafka.topics.partitions` and `kafka.topics.replication`, so the broker does not create topics on first use |
| `kafka.kafkaConfig.logRetentionHours` | `72` | How long records stay; writers normally consume them within seconds |

Other keys: the [chart's values](https://github.com/CloudPirates-io/helm-charts/blob/main/charts/kafka/values.yaml).

Turn TLS on for anything beyond evaluation: SASL PLAIN sends the password in clear text without it. With `self-signed` or
`cert-manager` FastProof reads the CA from `<release>-kafka-tls`; with `existing-secret` the certificate must
cover `<release>-kafka` and `*.<release>-kafka-headless.<namespace>.svc.cluster.local`, because clients
bootstrap through the Service and then connect to each broker by its pod name. A cert-manager issuer must put
the CA into `ca.crt` (a CA or self-signed issuer does).

The generated password and the self-signed certificate are kept on upgrades by reading the existing Secrets.
Tools that render the chart without cluster access (`helm template`, some GitOps controllers) would generate new
ones on every render; give them `kafka.auth.existingSecret` and a `cert-manager` or `existing-secret` source.

### Mirror registries

Every image can be pulled from a mirror. Each one takes a repository, tag and (where the chart supports it)
digest:

| Image | Values |
|---|---|
| FastProof | `image.repository`, `image.tag`, `image.digest` |
| Garage | `garage.image.repository`, `tag`, `digest` |
| Bundled ClickHouse | `sinks.clickhouse.bundled.image.repository`, `tag`, `digest` |
| CloudPirates PostgreSQL | `postgresql.image.registry`, `repository`, `tag` (tag may end in `@sha256:...`) |
| Valkey | `valkey.image.registry`, `repository`, `tag` |
| CloudNativePG | `cnpg.cluster.imageName` |
| CloudPirates Kafka | `kafka.image.registry`, `repository`, `tag` (tag may end in `@sha256:...`), `kafka.image.pullSecrets` |
| OpenSearch | `opensearch.image.repository`, `tag` (and `opensearch.imagePullSecrets`) |

`imagePullSecrets` applies to FastProof, the migration Job, Garage and ClickHouse; Kafka takes
`kafka.image.pullSecrets`. The chart's
`ci/mirror-values.yaml` shows every key with a mirror under one prefix.

## Main values

| Key | Default | Meaning |
|---|---|---|
| `image.repository`, `image.tag`, `image.digest` | `docker.io/franshub/fastproof`, app version, none | Image; set the digest to pin it |
| `replicaCount` | `1` | Replicas when autoscaling is off |
| `publicUrl` | none | External URL; required with OIDC |
| `server.logFormat`, `server.logFilter` | `json`, `info` | `FASTPROOF_LOG_FORMAT`, `RUST_LOG` |
| `server.maxUploadBytes`, `server.ingestWorkers` | 1 GiB, `2` | Ingest settings |
| `server.retainUploads`, `server.outboxRetentionDays` | `false`, `7` | `FASTPROOF_RETAIN_UPLOADS`, `FASTPROOF_OUTBOX_RETENTION_DAYS` |
| `server.ui` | `true` | Serve the web UI |
| `server.logs.maxLineBytes`, `maxLinesPerRequest`, `maxRequestBytes`, `maxBytesPerLaunch` | 64 KiB, `5000`, 8 MiB, 256 MiB | Live log limits (`FASTPROOF_LOG_MAX_*`) |
| `server.extraEnv`, `server.extraEnvFrom` | none | Additional environment |
| `database.*` | | External database URL, pool size, TLS |
| `migrations.mode` | `job` | `job`: a Helm hook Job runs `fastproof migrate`; `auto`: the server migrates at start |
| `blob.*`, `garage.*` | `fs` | Attachment storage |
| `spool.sizeLimit` | `4Gi` | `emptyDir` for uploads in progress (`FASTPROOF_SPOOL_DIR`) |
| `auth.adminTokens.existingSecret` / `tokens` / `generate` | none | Static tokens file |
| `auth.access.roles`, `auth.access.bindings` or `existingConfigMap` | none | Access file |
| `oidc.*` | off | OIDC sign-in; `clientSecret.existingSecret` (key `client-secret`); `insecureCookie` only for local development |
| `tls.existingSecret`, `tls.clientCa` | none | TLS on the main listener |
| `integrations.egressAllow` | `[]` | Hosts integrations may call (`FASTPROOF_EGRESS_ALLOW`); empty denies every outbound call |
| `integrations.secretsKey.existingSecret`, `key` | none | Key file of project secrets (`FASTPROOF_SECRETS_KEY_FILE`); `value` makes the chart create the Secret |
| `integrations.smtp.url`, `from`, `username`, `password.existingSecret` | none | Mail relay of email notifications (`FASTPROOF_SMTP_*`; the password is mounted as a file) |
| `integrations.telegramApiUrl` | none (`https://api.telegram.org`) | `FASTPROOF_TELEGRAM_API_URL` |
| `integrations.delivery.maxAttempts`, `backoffSeconds` | `6`, `30` | Notification retries (`FASTPROOF_DELIVERY_*`) |
| `otel.endpoint`, `otel.protocol` | none | OTLP traces |
| `service.port`, `service.opsPort` | `8080`, `9090` | Service ports `http` and `ops` |
| `httpRoute.*` | off | Gateway API HTTPRoute (`parentRefs`, `hostnames`, `timeouts`) |
| `ingress.*` | off | Ingress (`className`, `hosts`, `tls`, `annotations`) |
| `metrics.serviceMonitor.enabled` | `false` | Prometheus Operator ServiceMonitor on the `ops` port |
| `metrics.topBranches` | `10` | Branches per project with their own label in quality metrics (`FASTPROOF_METRICS_TOP_BRANCHES`) |
| `networkPolicy.*` | off | See below |
| `pdb.enabled`, `pdb.maxUnavailable` | `true`, `1` | PodDisruptionBudget, rendered with more than one replica |
| `autoscaling.*` | off | HorizontalPodAutoscaler on CPU (and memory) |
| `resources` | 100m/128Mi requests, 2 CPU/1Gi limits | Container resources, including ephemeral storage for the spool |
| `ingest.mode` | `direct` | `direct` or `kafka` |
| `search.backend` | `postgres` | `postgres`, `clickhouse` or `opensearch` |
| `roles.split`, `roles.<role>.*` | `false` | Separate Deployments per role |
| `kafka.*`, `sinks.*`, `opensearch.*` | off | Kafka, ClickHouse and OpenSearch; see above |
| `kafka.topics.replication`, `kafka.clientId`, `kafka.maxRecordBytes` | `-1`, `fastproof`, `900000` | `FASTPROOF_KAFKA_REPLICATION`, `_CLIENT_ID`, `_MAX_RECORD_BYTES` |
| `kafka.consumer.fetchMaxBytes`, `maxInflightBytes`, `batchMaxRecords`, `batchMaxBytes` | `1048576`, `67108864`, `1000`, `1048576` | `FASTPROOF_KAFKA_FETCH_MAX_BYTES`, `_MAX_INFLIGHT_BYTES`, `_BATCH_MAX_RECORDS`, `_BATCH_MAX_BYTES`; the fetch and the budget are raised to at least `kafka.maxRecordBytes`. See [memory of the Kafka consumers](scaling.md#memory-of-the-kafka-consumers) |
| `database.connectTimeoutSeconds`, `kafka.connectTimeoutSeconds` | `300`, `300` | How long pods wait at start for PostgreSQL and Kafka before they exit (`FASTPROOF_DATABASE_CONNECT_TIMEOUT_SECONDS`, the older name of `FASTPROOF_DATABASE_WAIT`, and `FASTPROOF_KAFKA_CONNECT_TIMEOUT_SECONDS`) |
| `sinks.batchSize`, `sinks.flushIntervalMs`, `sinks.opensearch.replicas` | `2000`, `1000`, `1` | `FASTPROOF_SINK_BATCH_SIZE`, `FASTPROOF_SINK_FLUSH_INTERVAL_MS`, `FASTPROOF_OPENSEARCH_REPLICAS` |

Every secret (database URL, S3 keys, token file, OIDC client secret, Kafka, ClickHouse and OpenSearch
passwords, the secrets key and the SMTP password) is mounted as a file under
`/etc/fastproof/secrets`; CAs under `/etc/fastproof/ca`. Use existing Secrets in production; values such as
`database.url` or `blob.s3.credentials.secretKey` make the chart create the Secret, which puts the secret in
the Helm release data.

The `ci/` directory of the chart has complete examples: `minimal-fs-values.yaml`,
`cloudpirates-garage-values.yaml`, `cnpg-external-s3-values.yaml`, `oidc-values.yaml`, `ha-values.yaml`,
`kafka-clickhouse-values.yaml`, `kafka-opensearch-values.yaml`, `split-roles-values.yaml`,
`bigdata-values.yaml` (a small-footprint big-data setup), `integrations-values.yaml` (egress allowlist,
secrets key, SMTP and delivery settings) and `mirror-values.yaml`.

The chart sets every server variable on [Configuration](configuration.md), from a value or from its own
layout (listeners, spool and blob directories, mounted file paths); `server.extraEnv` remains for anything
new before the chart catches up. The secrets key is read at start: after changing the Secret, restart
the Deployments (see [Secrets key rotation](configuration.md#secrets-key-rotation)).

## Exposing FastProof

Gateway API:

```yaml
publicUrl: https://fastproof.example.com
httpRoute:
  enabled: true
  parentRefs:
    - name: gateway
      namespace: gateway-system
  hostnames:
    - fastproof.example.com
  timeouts:
    request: 300s
```

Ingress:

```yaml
ingress:
  enabled: true
  className: nginx
  annotations:
    nginx.ingress.kubernetes.io/proxy-body-size: 1g
  hosts:
    - fastproof.example.com
  tls:
    - secretName: fastproof-tls
      hosts:
        - fastproof.example.com
```

Uploads can be large: allow request bodies up to `server.maxUploadBytes` and long enough timeouts at the
gateway. Never route port 9090 outside the cluster.

## NetworkPolicy

With `networkPolicy.enabled: true` the chart allows:

- ingress to port 8080 from `networkPolicy.ingress.from` (any source when empty), and to port 9090 only from
  the namespace selected by `networkPolicy.ingress.metrics.namespaceSelector` (default `monitoring`);
- egress (`networkPolicy.egress.enabled`, on by default) to DNS, to the bundled PostgreSQL, CloudNativePG,
  Valkey, Garage, Kafka, ClickHouse and OpenSearch pods, and to the peers you list for external services:
  `egress.database.to`, `egress.s3.to`, `egress.oidc.to`, `egress.valkey.to`, `egress.otlp.to`,
  `egress.kafka.to` (port 9092 by default), `egress.clickhouse.to` (8443) and `egress.opensearch.to` (9200),
  each with `ports`, plus `egress.extraRules`. The chart refuses to render when an external service is configured without peers.
  Integrations (webhooks, Slack, Microsoft Teams, Telegram, GitLab, GitHub, FastMock, the SMTP relay) need their own
  `egress.extraRules`, matching `integrations.egressAllow`.

`networkPolicy.storage` (on by default) restricts ingress to the bundled PostgreSQL, Valkey and Garage pods to
FastProof's own pods. Pods managed by CloudNativePG are excluded from it (`cnpg.io/cluster DoesNotExist`),
because they carry the same `app.kubernetes.io/name: postgresql` label and the operator must keep reaching
them. The bundled Kafka, ClickHouse and OpenSearch get their own policies: only FastProof pods reach their
client port, and Kafka and OpenSearch pods may also talk to themselves.

```yaml
networkPolicy:
  enabled: true
  ingress:
    from:
      - namespaceSelector:
          matchLabels:
            kubernetes.io/metadata.name: gateway-system
  egress:
    database:
      to:
        - ipBlock:
            cidr: 198.51.100.10/32
    s3:
      to:
        - ipBlock:
            cidr: 198.51.100.20/32
```

## Migrations and upgrades

Migrations are additive (expand and contract): a column or table is removed only one minor release after the
code stopped using it, indexes are built with `CREATE INDEX CONCURRENTLY`, and every query names its columns.
Old and new pods therefore run side by side during a rolling update.

Pods carry checksums of the chart's ConfigMap and of the secret values the chart writes into Secrets
(`checksum/config`, `checksum/secrets`), so `helm upgrade` with a changed setting or secret rolls them. Secrets
you manage yourself (`existingSecret`) are not tracked: restart the Deployment after changing one.

With `migrations.mode: job` (default):

- External database: a Job runs `fastproof migrate` as a `pre-install` and `pre-upgrade` hook, under its own
  ServiceAccount, and the server runs with `FASTPROOF_MIGRATE=off`. A failed migration stops the upgrade
  before any pod changes.
- Database installed with the release (`postgresql` or `cnpg`): it does not exist yet before the first
  install, so the first install lets the server migrate at start (`FASTPROOF_MIGRATE=auto`, under an advisory
  lock), and every upgrade runs the `pre-upgrade` Job.

With `migrations.mode: auto` there is no Job and every replica migrates at start under the advisory lock.

Pods that start before PostgreSQL (for example the bundled one on the first install) or Kafka wait for them
instead of crashing: `/healthz` answers, `/readyz` reports `starting`, and every attempt is logged. After
`database.connectTimeoutSeconds` (or `kafka.connectTimeoutSeconds`, default 300 each) without success the
pod exits with an error that names the setting.

Upgrade one minor version at a time and read the release notes and [Upgrading](upgrading.md). Downgrades are not supported once a newer
version has migrated the schema; restore a backup instead.

Releases that used the removed `kafka.bundled` values switch to `kafka.enabled: true`. The broker keeps the
name `<release>-kafka`, but its StatefulSet changes fields Kubernetes cannot update in place and its old volume
belongs to another cluster. Wait until the consumer lag is 0, delete the StatefulSet `<release>-kafka` and the
PVC `data-<release>-kafka-0`, then run `helm upgrade`; records still in the old broker are lost. FastProof keeps
consumer offsets in PostgreSQL, so on the new, empty topics each consumer logs
`Kafka offset out of range; resetting` once and continues from the end of each partition: hold uploads until the
writers are ready.

## Several replicas

Replicas are stateless and behave as one: the upload queue is in PostgreSQL (each upload is processed once,
by any replica), sessions, tokens and rate-limit windows are shared, and live updates reach every replica.

```yaml
replicaCount: 2
blob:
  backend: s3
valkey:
  enabled: true
autoscaling:
  enabled: true
  minReplicas: 2
  maxReplicas: 4
topologySpreadConstraints:
  - maxSkew: 1
    topologyKey: kubernetes.io/hostname
    whenUnsatisfiable: ScheduleAnyway
    labelSelector:
      matchLabels:
        app.kubernetes.io/name: fastproof
        app.kubernetes.io/component: server
```

- More than one replica needs S3 or a ReadWriteMany volume; the chart refuses a ReadWriteOnce volume.
- With a ReadWriteOnce volume the Deployment uses the `Recreate` strategy; otherwise rolling updates with
  `maxUnavailable: 0`.
- Size the database connections: replicas × `database.maxConnections` plus the migration Job must stay below
  the server's `max_connections` (the CloudNativePG values set 200).
- A PodDisruptionBudget (`maxUnavailable: 1`) is rendered when there is more than one replica.

### Separate roles

With `roles.split: true` the chart runs each role as its own Deployment, so the HTTP API, the parsers, the
Kafka writers and the sinks scale and fail independently:

| Deployment | `FASTPROOF_ROLES` | Rendered |
|---|---|---|
| `<release>-fastproof-api` | `api,scheduler` | Always; the Service, HTTPRoute and Ingress point here |
| `<release>-fastproof-ingest` | `ingest` | Always |
| `<release>-fastproof-writer` | `writer` | With `ingest.mode: kafka` |
| `<release>-fastproof-sink` | `sink` | With a ClickHouse or OpenSearch sink |

Each `roles.<role>` takes `replicaCount`, `extraRoles`, `resources`, probe overrides (`startupProbe`,
`readinessProbe`, `livenessProbe`, merged over the top-level ones), `autoscaling`, `pdb`, pod labels and
annotations, and scheduling (`nodeSelector`, `tolerations`, `affinity`, `topologySpreadConstraints`). Empty
values fall back to the top-level ones. The top-level `replicaCount` and `autoscaling` apply only without
the split. A headless `<release>-fastproof-ops` Service lets the ServiceMonitor scrape every role. Split roles
share attachments, so they need S3 or a ReadWriteMany volume.

`roles.writer` and `roles.sink` default to 192 MiB requested and 512 MiB limit, sized for the default
in-flight budget of the Kafka consumers (`kafka.consumer.maxInflightBytes`, 64 MiB). To run them smaller,
lower the budget first: `ci/bigdata-values.yaml` runs them in 384 MiB with a 32 MiB budget. See
[memory of the Kafka consumers](scaling.md#memory-of-the-kafka-consumers).

## Monitoring

Port 9090 serves `/metrics` (Prometheus), `/healthz` and `/readyz`. Enable
`metrics.serviceMonitor.enabled` with the Prometheus Operator, or scrape the `ops` port of the Service.

## Uninstall

`helm uninstall` removes FastProof and the bundled services. The attachment volume of `blob.backend: fs` and
the volumes of the bundled PostgreSQL (CloudPirates), Garage and Kafka StatefulSets are kept; delete the PVCs to
remove the data. A CloudNativePG cluster removes its own volumes when the `Cluster` is deleted.
