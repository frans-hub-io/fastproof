# Configuration

FastProof is configured with environment variables. Secrets can be given as files (`*_FILE` variables), which
is how the Helm chart passes them. A wrong or missing setting stops the server at start with an error that
names the variable.

## Listeners

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_LISTEN` | `0.0.0.0:8080` | Main listener: UI at `/`, API at `/api/v1`, sign-in at `/auth/*`, OpenAPI at `/api/openapi.json`, Swagger UI at `/api/docs`, `GET /healthz` |
| `FASTPROOF_OPS_LISTEN` | `0.0.0.0:9090` | Operations listener: `/metrics` (Prometheus), `/healthz` (process alive), `/readyz` (database reachable, migrations at the expected version, blob store reachable) |
| `FASTPROOF_PUBLIC_URL` | none | External base URL, for example `https://fastproof.example.com`. Required with OIDC |
| `FASTPROOF_TLS_CERT_FILE`, `FASTPROOF_TLS_KEY_FILE` | none | Serve TLS on the main listener (PEM files) |
| `FASTPROOF_TLS_CLIENT_CA_FILE` | none | Require client certificates signed by this CA on the main listener |
| `FASTPROOF_UI` | `on` | Serve the embedded web UI (`off` leaves the API only) |
| `FASTPROOF_SHUTDOWN_TIMEOUT_SECONDS` | `25` | Longest stop after `SIGTERM` (1 to 3600). The Helm chart sets it to `terminationGracePeriodSeconds` minus 5 |

Keep the operations port internal: it is unauthenticated by design. Expose only port 8080.

The operations listener answers from the first moment of start-up. While FastProof waits for PostgreSQL (and
Kafka in Kafka mode), `/healthz` returns 200 and `/readyz` returns 503 with `"status": "starting"`, so a pod
that starts before its database is not restarted by its liveness probe and gets no traffic.

On `SIGTERM` (or Ctrl-C) the listeners stop accepting connections, requests in flight finish, open event
streams (`/events`, `/logs/stream`) end so that browsers reconnect to another replica, ingest workers and
Kafka consumers finish the batch they hold, and the process exits; whatever still runs after
`FASTPROOF_SHUTDOWN_TIMEOUT_SECONDS` is cut off and picked up again after a restart (uploads are re-queued,
Kafka batches are read again from the committed offset).

## Database

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_DATABASE_URL` | required | PostgreSQL URL, for example `postgres://fastproof:secret@db.example.com:5432/fastproof?sslmode=verify-full` |
| `FASTPROOF_DATABASE_URL_FILE` | none | File holding the URL, instead of `FASTPROOF_DATABASE_URL` |
| `FASTPROOF_DATABASE_CA_FILE` | none | CA bundle to verify the PostgreSQL server certificate |
| `FASTPROOF_DATABASE_MAX_CONNECTIONS` | `20` | Connection pool size per replica |
| `PGSSLMODE`, `PGSSLROOTCERT` | none | Standard libpq variables, used when the URL sets no `sslmode` |
| `FASTPROOF_DATABASE_STATEMENT_TIMEOUT_SECONDS` | `120` | Longest SQL statement on pooled connections; `0` turns it off. Migrations and index builds run without it; the purge of deleted projects runs under it in short statements of at most 5 000 rows each, and a purge that stops continues on the next run |
| `FASTPROOF_DATABASE_INGEST_RESERVE` | a quarter of the pool, at least 1 | Connections that heavy reads (dashboard, log search, similar failures, rule dry runs, stored log reads) leave free for ingest and workers; must be below `FASTPROOF_DATABASE_MAX_CONNECTIONS` |
| `FASTPROOF_DATABASE_WAIT` | `5m` | How long `serve` (every role) and `migrate` wait at start for PostgreSQL (and, with `FASTPROOF_MIGRATE=off`, for the migrations) before they exit with an error: seconds (`300`) or `90s`, `5m`, `1h`, up to `24h`; `0` tries once. Attempts back off from 1 s to 30 s; each failed one is logged as one warning with `error_class` (`connection_refused`, `dns`, `timeout`, `tls`, `authentication`, `database_missing`, `too_many_connections`, `starting_up`, `database`, `io`, `other`). Meanwhile `/healthz` answers `200` and `/readyz` `503` `starting` |
| `FASTPROOF_DATABASE_CONNECT_TIMEOUT_SECONDS` | none | Older name of `FASTPROOF_DATABASE_WAIT` in seconds (0 to 86400), used when `FASTPROOF_DATABASE_WAIT` is not set |
| `FASTPROOF_MIGRATE` | `auto` | `auto` applies migrations at start under an advisory lock; `off` expects them applied by `fastproof migrate` (the Helm chart runs it as a Job) |

PostgreSQL 14 to 18 is supported; 18 is recommended. PostgreSQL 14 reached end of life in November 2026.

## Request guards

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_SSE_MAX_PER_CALLER` | `16` | Open event streams (`/events`, `/logs/stream`) per caller and replica; more answer 429 |
| `FASTPROOF_SSE_RECHECK_SECONDS` | `300` | How often an open stream checks that its caller still has access; it ends when not |
| `FASTPROOF_HEAVY_QUERIES_PER_CALLER` | `4` | Heavy reads running at once per caller and replica; more answer 429 |
| `FASTPROOF_MAX_CONCURRENT_SPOOLS` | `32` | Uploads and attachments written to the spool directory at once per replica; others wait up to 10 s, then 503 |
| `FASTPROOF_BODY_IDLE_TIMEOUT_SECONDS` | `60` | A request body that sends nothing for this long fails with 400 |

## Attachment storage

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_BLOB_BACKEND` | `fs` | `fs` (a directory) or `s3`. The Helm chart defaults to `s3` with the bundled Garage; the binary keeps `fs` so a plain `docker run` needs no storage service |
| `FASTPROOF_BLOB_DIR` | `/var/lib/fastproof/blobs` | Directory for `fs`. Several replicas need a shared (ReadWriteMany) volume |
| `FASTPROOF_S3_ENDPOINT` | none | S3-compatible endpoint, for example `https://s3.example.com` |
| `FASTPROOF_S3_REGION` | `us-east-1` | Signing region; must match the server's region (Garage defaults to `garage`) |
| `FASTPROOF_S3_BUCKET` | none | Bucket name |
| `FASTPROOF_S3_ACCESS_KEY_FILE`, `FASTPROOF_S3_SECRET_KEY_FILE` | none | Credentials from files |
| `FASTPROOF_S3_ACCESS_KEY`, `FASTPROOF_S3_SECRET_KEY` | none | Credentials inline, for local use |
| `FASTPROOF_S3_CA_FILE` | none | Custom CA for the S3 endpoint |
| `FASTPROOF_S3_PATH_STYLE` | `true` | Path-style addressing (`https://s3.example.com/bucket/key`) |

| `FASTPROOF_MEDIA_URL_TTL_SECONDS` | `300` | Lifetime of [signed media URLs](../reference/api.md#attachment-content-and-media-urls), 1 to 3600 |
| `FASTPROOF_MEDIA_URL_KEY`, `FASTPROOF_MEDIA_URL_KEY_FILE` | random per process | Secret (at least 16 characters) that is combined with the database secret into the signing key of media URLs. Set the same value on every replica; the Helm chart creates it once |
| `FASTPROOF_MEDIA_MAX_STREAMS` | `256` | Attachment bodies streamed at once per replica; more requests get `503` with `Retry-After` |
| `FASTPROOF_MEDIA_STREAM_TIMEOUT_SECONDS` | `60` | A body stream that makes no progress for this long is closed |
| `FASTPROOF_MEDIA_KEY_RELOAD_SECONDS` | `30` | How often replicas re-read the signing keys |
| `FASTPROOF_BLOB_BACKEND_MIGRATED` | `false` | Set to `true` for one start after the attachments were copied to a different blob store (see [Blob store identity](#blob-store-identity)) |
| `FASTPROOF_MEDIA_PREPARE` | `true` | Make [seekable copies of videos](../reference/api.md#seekable-videos) in the background (`media.prepare` job) |
| `FASTPROOF_MEDIA_PREPARE_MAX_BYTES` | `1073741824` | Larger videos keep only their original (at least 1024) |
| `FASTPROOF_MEDIA_PREPARE_TIMEOUT_SECONDS` | `300` | Time for one video, download and upload included, 10 to 3600; a video that takes longer keeps its original |

Attachments are content-addressed by SHA-256 and stored once per project. There is no key prefix setting:
FastProof uses the whole bucket, so give every installation its own bucket.

Content is read with ranged requests (`Range` on S3, a seek on `fs`), so a seek in a long video reads only
the bytes the player asks for from either backend.

### Media URL signing keys

The signing key of media URLs is derived (HKDF-SHA256) from two secrets: the row `media-url` of the
`stream_keys` table, created at first start and shared by all replicas, and `FASTPROOF_MEDIA_URL_KEY`, which
lives outside the database. A token names the key it was signed with, and a server accepts the current key and
the previous one (row `media-url-previous`); every replica re-reads both rows every
`FASTPROOF_MEDIA_KEY_RELOAD_SECONDS`. To rotate without a restart and without breaking URLs in use:

```sql
begin;
delete from stream_keys where name = 'media-url-previous';
update stream_keys set name = 'media-url-previous' where name = 'media-url';
commit;
```

Within the reload interval the replicas create a new `media-url` row, sign with it and keep accepting URLs of
the old one. Run it again after the longest URL lifetime to drop the old key. Changing `FASTPROOF_MEDIA_URL_KEY`
changes every key at once: URLs issued before stop working and players ask for new ones.

### Blob store identity

The server records which blob store the attachments live in (`fs:<directory>` or `s3:<endpoint>/<bucket>`) in
the table `blob_identity`. At start it compares the configured store with the record: while attachments exist,
a different store stops the start with a message that names both. The same check runs the first time after an
upgrade, when nothing is recorded yet: if none of the newest attachments can be found in the configured store,
the start is refused. After copying the attachments to a new store on purpose, start once with
`FASTPROOF_BLOB_BACKEND_MIGRATED=true` (Helm: `server.blobBackendMigrated: true`), then remove the setting.

## Ingest

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_SPOOL_DIR` | `/tmp/fastproof` | Temporary files of uploads being received and processed and of launch exports being built (an `emptyDir` in Kubernetes) |
| `FASTPROOF_LAUNCH_EXPORT_MAX_BYTES` | `2147483648` | Largest launch export file (uncompressed content); a bigger export fails with an error |
| `FASTPROOF_MAX_UPLOAD_BYTES` | `1073741824` (1 GiB) | Largest upload request |
| `FASTPROOF_INGEST_WORKERS` | `2` | Uploads processed at the same time per replica |
| `FASTPROOF_RETAIN_UPLOADS` | `false` | Keep the raw uploaded files in blob storage after processing (for re-parsing) |
| `FASTPROOF_OUTBOX_RETENTION_DAYS` | `7` | Days that ingest events stay in the PostgreSQL outbox for optional consumers |
| `FASTPROOF_OUTBOX_LOG_EVENTS` | `chunk` | How stored log lines are announced in the outbox (and the `events` topic): `chunk` writes one `log.chunk` event per stored chunk (every version reads it); `batch` writes one `log.chunks` event per launch and write that lists its chunks, far fewer outbox rows. Set `batch` on the writers once every sink replica and every outside consumer of `events` reads `log.chunks` (see [Upgrading](upgrading.md)) |

Give the spool directory room for `FASTPROOF_INGEST_WORKERS` uploads plus the ones being received. The other
ingest limits are listed on [Limits](limits.md).

## Ingest mode, roles and search

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_INGEST_MODE` | `direct` | `direct`: uploads go through the spool and a PostgreSQL job queue to the ingest workers. `kafka`: the HTTP gateway produces to Kafka and writer consumers persist |
| `FASTPROOF_ROLES` | `all` | Components this process runs: `all`, or a comma list of `api`, `ingest`, `writer`, `sink`, `scheduler` |
| `FASTPROOF_SEARCH_BACKEND` | `postgres` | Where log search and similar-failure lookups go: `postgres`, `clickhouse` or `opensearch` |
| `FASTPROOF_SINKS` | empty | Consumers that copy results, attempts, steps and logs out: empty, or a comma list of `clickhouse`, `opensearch` |

A search backend other than `postgres` needs the matching sink, because the sink is what fills it. See
[Scaling and big data](scaling.md) for when to use which.

## Kafka

Used only with `FASTPROOF_INGEST_MODE=kafka`.

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_KAFKA_BROKERS` | required | Bootstrap brokers, a comma list of `host:port` |
| `FASTPROOF_KAFKA_TLS` | `false` | Connect with TLS |
| `FASTPROOF_KAFKA_CA_FILE` | none | CA bundle for the brokers' certificates |
| `FASTPROOF_KAFKA_SASL_MECHANISM` | none | `plain`, `scram-sha-256` or `scram-sha-512` |
| `FASTPROOF_KAFKA_USERNAME` | none | SASL user |
| `FASTPROOF_KAFKA_PASSWORD_FILE` | none | File with the SASL password |
| `FASTPROOF_KAFKA_TOPIC_PREFIX` | `fastproof.` | Prefix of the topics `ingest.results`, `ingest.logs`, `ingest.uploads` and `events` |
| `FASTPROOF_KAFKA_CREATE_TOPICS` | `false` | Create missing topics at start, and again when a topic disappears while running (needs the Kafka `Create` permission) |
| `FASTPROOF_KAFKA_PARTITIONS` | `12` | Partitions of topics FastProof creates (1 to 1000) |
| `FASTPROOF_KAFKA_REPLICATION` | `-1` | Replication factor of topics FastProof creates; `-1` uses the broker default |
| `FASTPROOF_KAFKA_TRUSTED_PRODUCERS` | `false` | Accept records written straight to Kafka by other producers. Leave it off unless Kafka ACLs restrict who can write |
| `FASTPROOF_KAFKA_CLIENT_ID` | `fastproof` | Client id sent to the brokers (shows up in broker logs and quotas) |
| `FASTPROOF_KAFKA_MAX_RECORD_BYTES` | `900000` | Largest record FastProof produces (16 KiB to 64 MiB); keep it below the topic's `max.message.bytes` |
| `FASTPROOF_KAFKA_SIGNATURE_MAX_AGE_HOURS` | `168` | Records FastProof signed longer ago than this are rejected; signatures carry a key id and a time. Upgrade all replicas together: records signed by older versions are rejected |
| `FASTPROOF_KAFKA_FETCH_MAX_BYTES` | `1048576` (1 MiB), at least the record size | Decoded bytes one partition fetch aims at; from `FASTPROOF_KAFKA_MAX_RECORD_BYTES` to 64 MiB |
| `FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES` | `67108864` (64 MiB), at least the fetch size | Record bytes one process holds across all partitions it consumes; fetches wait while it is spent. From `FASTPROOF_KAFKA_FETCH_MAX_BYTES` to 64 GiB |
| `FASTPROOF_KAFKA_BATCH_MAX_RECORDS` | `1000` | Records handled and committed in one transaction, 1 to 100000 |
| `FASTPROOF_KAFKA_BATCH_MAX_BYTES` | `1048576` (1 MiB) | Record bytes handled and committed in one transaction (a larger record goes alone), 16 KiB to 64 MiB |
| `FASTPROOF_KAFKA_CONNECT_TIMEOUT_SECONDS` | `300` | How long start-up waits for Kafka (connection and topics) before it exits with an error; every attempt is logged. `0` tries once. 0 to 86400 |

Consumer offsets and partition leases live in PostgreSQL, so Kafka needs only topic `Read`, `Describe` and
`Write` for the FastProof user (plus `Create` when FastProof creates the topics). Use SASL over TLS outside a
trusted network.

The memory of the Kafka consumers depends on `FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES`, not on the lag or the
number of partitions: a writer that starts behind a large backlog drains it within the same memory. See
[memory of the Kafka consumers](scaling.md#memory-of-the-kafka-consumers) for how to size the limits.

## ClickHouse sink

Used when `FASTPROOF_SINKS` contains `clickhouse` or `FASTPROOF_SEARCH_BACKEND=clickhouse`.

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_CLICKHOUSE_URL` | required | HTTP interface URL, for example `https://clickhouse.example.com:8443` |
| `FASTPROOF_CLICKHOUSE_DATABASE` | `fastproof` | Database; FastProof creates its tables there |
| `FASTPROOF_CLICKHOUSE_USER` | none | User |
| `FASTPROOF_CLICKHOUSE_PASSWORD_FILE` | none | File with the password |
| `FASTPROOF_CLICKHOUSE_CA_FILE` | none | CA bundle for an `https://` URL |
| `FASTPROOF_CLICKHOUSE_RAW_TTL_DAYS` | `90` | Days raw rows (results, attempts, steps, logs) are kept; daily aggregates stay |

FastProof uses only the HTTP interface. The user needs all privileges on its database and nothing else; keep
the native port 9000 closed.

## OpenSearch sink

Used when `FASTPROOF_SINKS` contains `opensearch` or `FASTPROOF_SEARCH_BACKEND=opensearch`.

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_OPENSEARCH_URL` | required | Cluster URL, for example `https://opensearch.example.com:9200` |
| `FASTPROOF_OPENSEARCH_USER` | none | User |
| `FASTPROOF_OPENSEARCH_PASSWORD_FILE` | none | File with the password |
| `FASTPROOF_OPENSEARCH_CA_FILE` | none | CA bundle for the cluster certificate |
| `FASTPROOF_OPENSEARCH_INDEX_PREFIX` | `fastproof-` | Prefix of index templates, data streams (`<prefix>logs-*`) and the ISM policy |
| `FASTPROOF_OPENSEARCH_RAW_TTL_DAYS` | `90` | Days raw documents are kept (through an ISM policy) |
| `FASTPROOF_OPENSEARCH_REPLICAS` | `1` | Replicas of the indexes FastProof creates; `0` on a single-node cluster |

OpenSearch 2.19 and 3.x are supported. The user needs `indices_all` on `<prefix>*` and `.ds-<prefix>*`, index
template and ISM policy management, and `cluster_monitor`.

## Sink batching

Apply to every sink (`FASTPROOF_SINKS`).

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_SINK_BATCH_SIZE` | `2000` | Rows (or documents) written per request, 1 to 100000 |
| `FASTPROOF_SINK_FLUSH_INTERVAL_MS` | `1000` | Longest wait before a partial batch is written, 10 to 600000 |

Larger batches cost fewer requests; a shorter interval makes search see new data sooner. Whatever the batch
size, a sink loads and writes at most 1000 results, 250 attempts or about 4 MiB of log lines at a time, and
each sink runs at most two writes at once per process.

## Live logs

Limits of `POST /api/v1/projects/{key}/launches/{id}/logs` (see [Live logs](../guide/live-logs.md)).

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_LOG_MAX_LINE_BYTES` | `65536` (64 KiB) | Largest message of one line, 256 to 1048576 |
| `FASTPROOF_LOG_MAX_LINES_PER_REQUEST` | `5000` | Lines in one request, 1 to 100000 |
| `FASTPROOF_LOG_MAX_REQUEST_BYTES` | `8388608` (8 MiB) | Largest request body, 1024 to 268435456 |
| `FASTPROOF_LOG_MAX_BYTES_PER_LAUNCH` | `268435456` (256 MiB) | Log bytes accepted per launch; later requests get `413` |

A message over the line limit is cut at a character boundary and ends with ` …[truncated]`; the line is
stored. A request over the line-count, request or per-launch limit gets `413` naming the limit.

## Test management

Limits of test cases, plans, manual runs and imports are set with `FASTPROOF_TM_<NAME>` variables, for example
`FASTPROOF_TM_MAX_FOLDERS=50000`. Every variable, its default and its maximum are listed in
[Limits](limits.md#test-management). Code sync and the backfill of cases run on the replicas with the
`scheduler` role (jobs `cases.sync` and `cases.backfill`). Import and export jobs start on the replica that
received the request and are picked up by the job `cases.jobs` (also on `scheduler` replicas) when that replica
is busy or stopped; their files are kept in the blob store under `tm-jobs/` until the job is over (exports: 24
hours).

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_TESTOPS_ALLOW_HTTP` | `false` | Allow `http` base URLs for imports from the Allure TestOps API; otherwise only `https`. The host must still be in `FASTPROOF_EGRESS_ALLOW`, and the import needs `FASTPROOF_SECRETS_KEY_FILE` |

## Access

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_ADMIN_TOKENS_FILE` | none | Static tokens for bootstrap, one per line: `name:role:sha256:<hex>` |
| `FASTPROOF_ACCESS_FILE` | none | YAML file with custom roles and bindings of users, OIDC groups and tokens |

At least static tokens or OIDC must be configured. See [Security](security.md) for roles, permissions and the
access file.

## OIDC sign-in

OIDC is off until `FASTPROOF_OIDC_ISSUER` is set; then the other variables apply.

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_OIDC_ISSUER` | off | Issuer URL, compared with `iss` byte for byte |
| `FASTPROOF_OIDC_CLIENT_ID` | required | Client id; the audience of ID tokens |
| `FASTPROOF_OIDC_CLIENT_SECRET_FILE` | | File with the client secret (or `FASTPROOF_OIDC_CLIENT_SECRET` inline) |
| `FASTPROOF_OIDC_REDIRECT_URL` | required | `<public URL>/auth/callback` |
| `FASTPROOF_OIDC_SCOPES` | `openid profile email` | Requested scopes; must include `openid` |
| `FASTPROOF_OIDC_USERNAME_CLAIM` | `preferred_username` | Claim (or dotted path) with the user name |
| `FASTPROOF_OIDC_GROUPS_CLAIM` | `groups` | Claim (or dotted path such as `realm_access.roles`) with the groups |
| `FASTPROOF_OIDC_ALLOWED_ALGS` | `RS256,PS256,ES256,EdDSA` | Accepted signature algorithms |
| `FASTPROOF_OIDC_API_AUDIENCE` | off | Audience of bearer JWTs accepted on the API; must differ from the client id |
| `FASTPROOF_OIDC_CA_FILE` | none | Extra CA bundle for the provider |
| `FASTPROOF_OIDC_SESSION_TTL` | `8h` | Absolute session lifetime (`30m`, `8h`, `1d`) |
| `FASTPROOF_OIDC_SESSION_IDLE` | `1h` | Session idle timeout |
| `FASTPROOF_OIDC_ALLOWED_HOSTS` | none | Hosts besides the issuer's that discovery may point to |
| `FASTPROOF_OIDC_COOKIE_NAME` | `franshub` | Base name of the session cookies |
| `FASTPROOF_OIDC_INSECURE_COOKIE` | `false` | Cookies without `Secure`; only for a loopback redirect URL in development |
| `FASTPROOF_OIDC_POST_LOGOUT_REDIRECT` | public URL | Where the provider sends the browser after logout |

## Integrations

Quality gates, notifications, merge request comments and the FastMock correlation are configured per project
(see [Quality gates](../guide/quality-gates.md) and [Notifications](../guide/notifications.md)). The server
settings below decide what they may reach.

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_EGRESS_ALLOW` | empty (nothing) | Hosts that integrations may call: a comma list of `host`, `*.domain`, `*`, IP addresses, each optionally with `:port`. Empty denies every outbound call |
| `FASTPROOF_SECRETS_KEY_FILE` | none | File with the keys that encrypt project secrets (AES-256-GCM), one per line: 64 hex characters or base64 of 32 bytes. The first line encrypts, every line decrypts. Without it the secrets API answers `503` |
| `FASTPROOF_SMTP_URL` | none | Mail relay of email notifications: `smtps://host:465` (TLS), `smtp://host:587` (STARTTLS, required) or `smtp+insecure://host:25` (no TLS, for a relay on a trusted network). No credentials in the URL |
| `FASTPROOF_SMTP_FROM` | required with SMTP | Sender, for example `FastProof <fastproof@example.com>` |
| `FASTPROOF_SMTP_USERNAME` | none | SMTP user; set together with the password |
| `FASTPROOF_SMTP_PASSWORD_FILE` | none | File with the SMTP password (or `FASTPROOF_SMTP_PASSWORD` inline) |
| `FASTPROOF_TELEGRAM_API_URL` | `https://api.telegram.org` | Telegram Bot API base URL, for a proxy or a self-hosted Bot API server |
| `FASTPROOF_DELIVERY_MAX_ATTEMPTS` | `6` | Attempts per notification or merge request update, 1 to 20 |
| `FASTPROOF_DELIVERY_BACKOFF_SECONDS` | `30` | First retry delay, doubled per attempt, at most one hour (0 to 3600) |
| `FASTPROOF_METRICS_TOP_BRANCHES` | `10` | Branches per project with their own `branch` label in the quality metrics; the rest are `other` (1 to 100) |

Every outbound call of an integration (webhooks, Slack, Microsoft Teams, Telegram, GitLab, GitHub, FastMock, and
the SMTP relay)
goes through the egress allowlist. A name that only matches a wildcard must resolve to public addresses; list
an internal host exactly to allow it. An entry with a port (`gitlab.example.com:443`) allows that port only:
other ports of the host are allowed only by another entry, and a wildcard entry never reaches an internal
address. Redirects are checked hop by hop (credential headers are dropped when a redirect leaves the origin;
GitLab, GitHub and FastMock calls do not follow redirects) and proxies from the environment are ignored. Allow only the hosts your projects need, for example
`FASTPROOF_EGRESS_ALLOW=hooks.slack.com,api.telegram.org,gitlab.example.com,smtp.example.com:465`. Microsoft
Teams workflow URLs are on `*.environment.api.powerplatform.com:443` (and `*.logic.azure.com:443` for
workflows built in Azure Logic Apps); see [Microsoft Teams](../guide/notifications.md#microsoft-teams-workflows).

Generate a secrets key with `openssl rand -hex 32` and keep it in your secret store. Only replicas with the
`scheduler` role re-encrypt secrets; every replica that serves the API needs the same key file.

### Secrets key rotation

The key file may hold several keys. The first line encrypts new values, every line decrypts, and replicas with
the `scheduler` role re-encrypt every secret with the first key when they start. A key's id is the first
16 hex characters of the SHA-256 of the raw 32 bytes (for a hex key:
`echo -n <key> | xxd -r -p | sha256sum | cut -c1-16`); the server logs it after re-encrypting.

1. Add the new key as the second line of the key file and roll out (for example
   `kubectl rollout restart deployment/<release>-fastproof`). Every replica can now decrypt values written with
   either key.
2. Move the new key to the first line and roll out again. Replicas encrypt with the new key, and the scheduler
   replicas re-encrypt the existing secrets at start.
3. Check that nothing uses the old key any more:
   `select count(*) from project_secrets where key_id <> '<active id>'` returns `0`. Then remove the old line
   and roll out.

Do not remove a key while that count is above zero: secrets encrypted only with a removed key can no longer be
read, and the server logs a warning about them.

## Live updates

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_VALKEY_URL` | none | Optional Valkey (or Redis 7.4) URL for live-update fan-out between replicas, for example `redis://default:secret@valkey:6379/0`. Without it PostgreSQL `LISTEN/NOTIFY` is used |

## Logs and telemetry

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_LOG_FORMAT` | `json` | `json` or `text` |
| `RUST_LOG` | `info` | Log filter, for example `info,fastproof=debug` |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | none | Send traces over OTLP; the standard `OTEL_EXPORTER_OTLP_*` and `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES` variables apply |

Secrets never appear in logs; tokens and passwords in URLs and headers are redacted.

`/metrics` on the operations listener carries `fastproof_build_info{version="..."}` (the release version, the
same as `fastproof version` and `GET /api/v1/version`) and, on Linux, the standard process metrics
`process_resident_memory_bytes`, `process_virtual_memory_bytes`, `process_cpu_seconds_total`,
`process_open_fds` and `process_start_time_seconds`, in every role.

## CLI

The same binary is the CLI. `fastproof` without a command prints the help; `fastproof <command> --help`
lists every option.

| Command | What it does |
|---|---|
| `serve` | Run the server (configured by the variables on this page) |
| `migrate` | Apply database migrations and exit |
| `upload <path>` | Upload a report file, an archive or a results directory into a launch. A launch without an external id is created for this upload and finished after processing (`--no-finish` keeps it open); a launch with an external id is shared and stays open (`--finish` finishes it). `--wait` waits for processing and prints a summary |
| `watch <dir>` | Upload new result files from a directory while tests run (`--finish` finishes the launch when stopped) |
| `launch start` | Start a launch, or join the one with the same external id, and print its id |
| `launch finish` | Wait for queued and processing uploads of the launch (`--timeout`, default 600 s; `--no-wait` skips it and reports how many are pending), finish it and print its counts |
| `gate` | Print the quality gate of a launch; exit `0` passed, `1` failed, `2` error or still pending (`--wait 10m` waits while it is pending) |
| `testplan` | Print an Allure `testplan.json` of chosen tests, so Allure adapters and the FastProof reporters run only those: the failed ones of a launch (`--from-launch <id> --status failed,broken`), the automated cases of a [test plan](../guide/manual-runs.md) (`--plan <id>`) or of a launch started from one (`--launch <externalId>`) |
| `cases export`, `cases import` | Export test cases as CSV or NDJSON (`--format`, `--out`); import a file, as a dry run unless `--apply` is given (see [Importing test cases](../guide/importing-test-cases.md)) |
| `bench` | Generate launches, results and log lines against a server and measure throughput and end-to-end lag (see [Scaling](scaling.md)) |
| `bench tm` | Load test management (cases, versions, code sync, reads, a plan launch run by virtual testers, export, a TestRail CSV import) in a new project and print p50/p95/max per operation against the lab targets; the project is deleted afterwards unless `--keep` (see [Scaling](scaling.md#test-management)) |
| `version` | Print the version |

The CLI reads:

| Variable | Meaning |
|---|---|
| `FASTPROOF_URL` | Server URL |
| `FASTPROOF_TOKEN`, `FASTPROOF_TOKEN_FILE` | API token, or a file holding it |
| `FASTPROOF_PROJECT` | Project key |
| `FASTPROOF_LAUNCH` | External launch id; uploads with the same id join one launch |
| `FASTPROOF_LAUNCH_NAME` | Launch name |
| `FASTPROOF_ENVIRONMENT` | Environment key, part of the test identity |
| `FASTPROOF_NO_CI` | `true` ignores the CI variables, like `--no-ci`: no launch id, name, CI or VCS metadata derived from them |
| `FASTPROOF_LOG_FORMAT` | `json` or `text` for the CLI's own messages |

`fastproof bench --kafka` also reads the `FASTPROOF_KAFKA_*` variables.

`fastproof migrate` needs only the database variables above (`FASTPROOF_DATABASE_*`, `PGSSLMODE`,
`PGSSLROOTCERT`) and exits when the schema is up to date.
