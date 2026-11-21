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
| `server.extraEnv`, `server.extraEnvFrom` | none | Additional environment |
| `database.*` | | External database URL, pool size, TLS |
| `migrations.mode` | `job` | `job`: a Helm hook Job runs `fastproof migrate`; `auto`: the server migrates at start |
| `blob.*`, `garage.*` | `fs` | Attachment storage |
| `spool.sizeLimit` | `4Gi` | `emptyDir` for uploads in progress (`FASTPROOF_SPOOL_DIR`) |
| `auth.adminTokens.existingSecret` / `tokens` / `generate` | none | Static tokens file |
| `auth.access.roles`, `auth.access.bindings` or `existingConfigMap` | none | Access file |
| `oidc.*` | off | OIDC sign-in; `clientSecret.existingSecret` (key `client-secret`) |
| `tls.existingSecret`, `tls.clientCa` | none | TLS on the main listener |
| `otel.endpoint`, `otel.protocol` | none | OTLP traces |
| `service.port`, `service.opsPort` | `8080`, `9090` | Service ports `http` and `ops` |
| `httpRoute.*` | off | Gateway API HTTPRoute (`parentRefs`, `hostnames`, `timeouts`) |
| `ingress.*` | off | Ingress (`className`, `hosts`, `tls`, `annotations`) |
| `metrics.serviceMonitor.enabled` | `false` | Prometheus Operator ServiceMonitor on the `ops` port |
| `networkPolicy.*` | off | See below |
| `pdb.enabled`, `pdb.maxUnavailable` | `true`, `1` | PodDisruptionBudget, rendered with more than one replica |
| `autoscaling.*` | off | HorizontalPodAutoscaler on CPU (and memory) |
| `resources` | 100m/128Mi requests, 2 CPU/1Gi limits | Container resources, including ephemeral storage for the spool |

Every secret (database URL, S3 keys, token file, OIDC client secret) is mounted as a file under
`/etc/fastproof/secrets`; CAs under `/etc/fastproof/ca`. Use existing Secrets in production; values such as
`database.url` or `blob.s3.credentials.secretKey` make the chart create the Secret, which puts the secret in
the Helm release data.

The `ci/` directory of the chart has complete examples: `minimal-fs-values.yaml`,
`cloudpirates-garage-values.yaml`, `cnpg-external-s3-values.yaml`, `oidc-values.yaml`, `ha-values.yaml`.

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
  Valkey and Garage pods, and to the peers you list for external services: `egress.database.to`,
  `egress.s3.to`, `egress.oidc.to`, `egress.valkey.to`, `egress.otlp.to` (each with `ports`), plus
  `egress.extraRules`. The chart refuses to render when an external service is configured without peers.

`networkPolicy.storage` (on by default) restricts ingress to the bundled PostgreSQL, Valkey and Garage pods to
FastProof's own pods. Pods managed by CloudNativePG are excluded from it (`cnpg.io/cluster DoesNotExist`),
because they carry the same `app.kubernetes.io/name: postgresql` label and the operator must keep reaching
them.

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

With `migrations.mode: job` (default):

- External database: a Job runs `fastproof migrate` as a `pre-install` and `pre-upgrade` hook, under its own
  ServiceAccount, and the server runs with `FASTPROOF_MIGRATE=off`. A failed migration stops the upgrade
  before any pod changes.
- Database installed with the release (`postgresql` or `cnpg`): it does not exist yet before the first
  install, so the first install lets the server migrate at start (`FASTPROOF_MIGRATE=auto`, under an advisory
  lock), and every upgrade runs the `pre-upgrade` Job.

With `migrations.mode: auto` there is no Job and every replica migrates at start under the advisory lock.

Upgrade one minor version at a time and read the release notes. Downgrades are not supported once a newer
version has migrated the schema; restore a backup instead.

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

## Monitoring

Port 9090 serves `/metrics` (Prometheus), `/healthz` and `/readyz`. Enable
`metrics.serviceMonitor.enabled` with the Prometheus Operator, or scrape the `ops` port of the Service.

## Uninstall

`helm uninstall` removes FastProof and the bundled services. The attachment volume of `blob.backend: fs` and
the volumes of the bundled PostgreSQL (CloudPirates) and Garage StatefulSets are kept; delete the PVCs to
remove the data. A CloudNativePG cluster removes its own volumes when the `Cluster` is deleted.
