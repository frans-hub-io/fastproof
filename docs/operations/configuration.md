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

Keep the operations port internal: it is unauthenticated by design. Expose only port 8080.

## Database

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_DATABASE_URL` | required | PostgreSQL URL, for example `postgres://fastproof:secret@db.example.com:5432/fastproof?sslmode=verify-full` |
| `FASTPROOF_DATABASE_URL_FILE` | none | File holding the URL, instead of `FASTPROOF_DATABASE_URL` |
| `FASTPROOF_DATABASE_CA_FILE` | none | CA bundle to verify the PostgreSQL server certificate |
| `FASTPROOF_DATABASE_MAX_CONNECTIONS` | `20` | Connection pool size per replica |
| `PGSSLMODE`, `PGSSLROOTCERT` | none | Standard libpq variables, used when the URL sets no `sslmode` |
| `FASTPROOF_MIGRATE` | `auto` | `auto` applies migrations at start under an advisory lock; `off` expects them applied by `fastproof migrate` (the Helm chart runs it as a Job) |

PostgreSQL 14 to 18 is supported; 18 is recommended. PostgreSQL 14 reached end of life in November 2026.

## Attachment storage

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_BLOB_BACKEND` | `fs` | `fs` (a directory) or `s3` |
| `FASTPROOF_BLOB_DIR` | `/var/lib/fastproof/blobs` | Directory for `fs`. Several replicas need a shared (ReadWriteMany) volume |
| `FASTPROOF_S3_ENDPOINT` | none | S3-compatible endpoint, for example `https://s3.example.com` |
| `FASTPROOF_S3_REGION` | `us-east-1` | Signing region; must match the server's region (Garage defaults to `garage`) |
| `FASTPROOF_S3_BUCKET` | none | Bucket name |
| `FASTPROOF_S3_ACCESS_KEY_FILE`, `FASTPROOF_S3_SECRET_KEY_FILE` | none | Credentials from files |
| `FASTPROOF_S3_ACCESS_KEY`, `FASTPROOF_S3_SECRET_KEY` | none | Credentials inline, for local use |
| `FASTPROOF_S3_CA_FILE` | none | Custom CA for the S3 endpoint |
| `FASTPROOF_S3_PATH_STYLE` | `true` | Path-style addressing (`https://s3.example.com/bucket/key`) |

Attachments are content-addressed by SHA-256 and stored once per project. There is no key prefix setting:
FastProof uses the whole bucket, so give every installation its own bucket.

## Ingest

| Variable | Default | Meaning |
|---|---|---|
| `FASTPROOF_SPOOL_DIR` | `/tmp/fastproof` | Temporary files of uploads being received and processed (an `emptyDir` in Kubernetes) |
| `FASTPROOF_MAX_UPLOAD_BYTES` | `1073741824` (1 GiB) | Largest upload request |
| `FASTPROOF_INGEST_WORKERS` | `2` | Uploads processed at the same time per replica |
| `FASTPROOF_RETAIN_UPLOADS` | `false` | Keep the raw uploaded files in blob storage after processing (for re-parsing) |
| `FASTPROOF_OUTBOX_RETENTION_DAYS` | `7` | Days that ingest events stay in the PostgreSQL outbox for optional consumers |

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
| `FASTPROOF_KAFKA_CREATE_TOPICS` | `false` | Create missing topics at start (needs the Kafka `Create` permission) |
| `FASTPROOF_KAFKA_PARTITIONS` | `12` | Partitions of topics FastProof creates (1 to 1000) |
| `FASTPROOF_KAFKA_REPLICATION` | `-1` | Replication factor of topics FastProof creates; `-1` uses the broker default |
| `FASTPROOF_KAFKA_TRUSTED_PRODUCERS` | `false` | Accept records written straight to Kafka by other producers. Leave it off unless Kafka ACLs restrict who can write |
| `FASTPROOF_KAFKA_CLIENT_ID` | `fastproof` | Client id sent to the brokers (shows up in broker logs and quotas) |
| `FASTPROOF_KAFKA_MAX_RECORD_BYTES` | `900000` | Largest record FastProof produces (16 KiB to 64 MiB); keep it below the topic's `max.message.bytes` |

Consumer offsets and partition leases live in PostgreSQL, so Kafka needs only topic `Read`, `Describe` and
`Write` for the FastProof user (plus `Create` when FastProof creates the topics). Use SASL over TLS outside a
trusted network.

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

Larger batches cost fewer requests and more memory per sink replica; a shorter interval makes search see new
data sooner.

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

Every outbound call of an integration (webhooks, Slack, Telegram, GitLab, GitHub, FastMock, and the SMTP relay)
goes through the egress allowlist. A name that only matches a wildcard must resolve to public addresses; list
an internal host exactly to allow it. An entry with a port (`gitlab.example.com:443`) allows that port only:
other ports of the host are allowed only by another entry, and a wildcard entry never reaches an internal
address. Redirects are checked hop by hop (credential headers are dropped when a redirect leaves the origin;
GitLab, GitHub and FastMock calls do not follow redirects) and proxies from the environment are ignored. Allow only the hosts your projects need, for example
`FASTPROOF_EGRESS_ALLOW=hooks.slack.com,api.telegram.org,gitlab.example.com,smtp.example.com:465`.

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
| `testplan` | Print an Allure `testplan.json` of chosen tests, for example the failed ones of a launch (`--from-launch <id> --status failed,broken`), so Allure adapters and the FastProof reporters run only those |
| `bench` | Generate launches, results and log lines against a server and measure throughput and end-to-end lag (see [Scaling](scaling.md)) |
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
| `FASTPROOF_LOG_FORMAT` | `json` or `text` for the CLI's own messages |

`fastproof bench --kafka` also reads the `FASTPROOF_KAFKA_*` variables.

`fastproof migrate` needs only the database variables above (`FASTPROOF_DATABASE_*`, `PGSSLMODE`,
`PGSSLROOTCERT`) and exits when the schema is up to date.
