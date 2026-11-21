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

Attachments are content-addressed by SHA-256 and stored once per project.

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

The same binary is the CLI (`fastproof upload`, `watch`, `launch start`, `launch finish`, `migrate`,
`version`). The CLI reads:

| Variable | Meaning |
|---|---|
| `FASTPROOF_URL` | Server URL |
| `FASTPROOF_TOKEN` | API token |
| `FASTPROOF_PROJECT` | Project key |
| `FASTPROOF_LAUNCH` | External launch id; uploads with the same id join one launch |

`fastproof migrate` uses the database variables above and exits when the schema is up to date.
