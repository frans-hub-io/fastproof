# API

Everything the UI and the CLI do goes through the HTTP API at `/api/v1`. It is described by OpenAPI:

- [API reference (Swagger UI)](../api/swagger/index.html) for the current release, on this site;
- `GET /api/openapi.json` and the Swagger UI at `/api/docs` on your own server, matching its version exactly.

## Conventions

- **Authentication:** `Authorization: Bearer <token>` with a managed token (`fp_...`), a static token or an
  OIDC access token (when `FASTPROOF_OIDC_API_AUDIENCE` is set). Browser sessions use cookies plus the
  `X-FransHub-Request: 1` header on writes.
- **JSON** field names are camelCase. Ids are UUIDs (version 7). Project keys match
  `^[a-z0-9][a-z0-9-]{1,62}$`.
- **Errors** have one shape: `{"code": "...", "message": "...", "details": {...}}`. `401` never tells why a
  credential was refused; `403` names the missing permission in `details.required`.
- **Lists** return `{"items": [...], "next": "..."}`. Pass `next` as `before` to get the next page; `limit`
  defaults to 50, at most 500. The tokens list is the exception, see [Tokens](#tokens).
- **Limits** answer `413` with a message that names the limit and its unit, for example
  `the body exceeds max_upload_bytes (max 1073741824 bytes)`, and `details` `{"limit", "max"}`. See
  [Limits](../operations/limits.md).

## Main resources

| Resource | Endpoints |
|---|---|
| Projects | `GET/POST /api/v1/projects`, `GET/PATCH/DELETE /api/v1/projects/{key}`, `GET/PUT .../settings` (see [Project settings](#project-settings)) |
| Launches | `POST /api/v1/projects/{key}/launches` (idempotent by `externalId`), `GET .../launches` with filters `branch`, `environment`, `status`, `attr=k:v`, `q`, `debug`; `GET/DELETE .../launches/{id}`, `POST .../launches/{id}/finish` |
| Uploads | `POST .../launches/{id}/uploads?format=auto\|allure\|junit\|ctrf\|playwright&fileName=` (raw body or multipart field `file`, answers `202`), `GET .../launches/{id}/uploads` |
| Streaming results | `POST .../launches/{id}/results` (FastProof JSON: one object, an array, or NDJSON); `POST/GET .../launches/{id}/logs`, `GET .../logs/stream` (see [Live logs](../guide/live-logs.md)) |
| Attachments | `POST .../launches/{id}/attachments`, `GET /api/v1/projects/{key}/attachments/{id}/content` |
| Results | `GET .../launches/{id}/results` with `status`, `q`, `suite`, `label=k:v`, `flaky`, `fingerprint` (items list the `clientIds` of their attempts, the ids reporters send and live log lines use); `GET .../launches/{id}/tree?groupBy=suite\|feature\|package` (with `includeTests=true` the groups carry their tests as leaves, `testCount` and `durationMs`, filtered by `status`, `q` and `flaky`; groups that do not fit one response, about 5000 tests, are read page by page from `GET .../launches/{id}/tree/tests?path=<JSON array>`); `GET /api/v1/projects/{key}/results/{id}` (with `muteUntil` and `muteReason` when muted) |
| Triage and analysis | defect types, `POST .../triage`, clusters, rules, flaky tests, compare, dashboard, test plans (see [Rules and triage](../guide/rules-and-triage.md)) |
| Integrations | `GET .../launches/{id}/gate`, project secrets, deliveries (see [Quality gates](../guide/quality-gates.md), [Notifications](../guide/notifications.md)) |
| History | `GET /api/v1/projects/{key}/test-cases/{testCaseId}/history?branch=&environment=` |
| Live events | `GET /api/v1/projects/{key}/events` (Server-Sent Events: `launch.updated`, `upload.updated`) |
| Identity and tokens | `GET /api/v1/me`, `GET/POST /api/v1/tokens`, `GET /api/v1/tokens/options`, `GET /api/v1/tokens/self`, `GET/DELETE /api/v1/tokens/{id}` (see [Tokens](#tokens)) |
| Audit | `GET /api/v1/audit?action=&principal=&scope=&before=&limit=` (see [Audit log](#audit-log)) |

## Project settings

`GET /api/v1/projects/{key}/settings` returns the whole settings document of a project (needs
`projects:read`). `PUT` (needs `settings:write`) updates it by top-level area:

- each area present in the body replaces the stored area as a whole: `flaky`, `retention`, `rollup`, `gates`,
  `notifications`, `issueTrackers`, `fastmock`, `vcs`;
- areas left out of the body are kept as they are;
- `null` resets an area to its default (for lists and `fastmock`/`vcs`: removes it).

The answer is the whole stored document. So a script can change one area without reading the others first:

```sh
curl -fsS -X PUT "$api/settings" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"retention":{"rawDays":30,"keepImportant":true}}'
```

To change one gate or one notification, read the document, edit the list and send the whole list back.

## Tokens

Managed tokens are created in the UI (**API tokens**) or through the API; the value starts with `fp_` and is
shown once. Request and response shapes are in the OpenAPI document (tag `admin`).

```sh
curl -fsS -X POST https://fastproof.example.com/api/v1/tokens -H "$auth" -H 'Content-Type: application/json' \
  -d '{"name":"ci-web","role":"uploader","scopes":["web"],"expiresInDays":30,"description":"CI of the web shop"}'
```

| Field | Meaning |
|---|---|
| `name` | 1 to 128 characters of `[A-Za-z0-9._@-]`, unique among active tokens |
| `role` | `viewer`, `uploader`, `editor`, `admin` or a custom role |
| `scopes` | Project keys, or `["*"]` for every project including later ones; or set `global: true` instead |
| `expiresInDays` | 1 to 365. Every token expires: without this field the token is valid for 90 days |
| `description` | Optional, at most 1024 bytes |

The answer is `201` with the metadata and `token`, the value. `GET /api/v1/tokens` answers
`{"tokens": [...]}`, newest first and not paged, optionally filtered by `scope` (a project key) and `status`
(`active`, `expired`, `revoked`). `GET /api/v1/tokens/options` lists the lifetimes (`defaultDays`, `maxDays`),
projects and roles the caller may grant. `DELETE /api/v1/tokens/{id}` revokes a token and the tokens it
created; a token may always revoke itself. A token can grant only permissions its creator holds where it
grants them, and a token created by a managed token expires no later than its creator.

## Audit log

`GET /api/v1/audit` (needs `audit:read`; with `scope=<project>` the permission in that project is enough)
returns records newest first: who (`principal`, its kind and roles), when, the project (`scope`), the
`action` and a `detail` object. `action` filters by an exact action or a prefix such as `rule.*`. Recorded
actions:

| Area | Actions |
|---|---|
| Projects and settings | `project.create`, `project.update`, `project.delete`, `project.settings` |
| Launches and uploads | `launch.create` (a new launch, not a joined one), `launch.update`, `launch.finish`, `launch.delete`, `upload.create` |
| Triage and rules | `results.triage`, `triage.apply` (through MCP), `defectType.create`, `defectType.update`, `defectType.delete`, `rule.create`, `rule.update`, `rule.delete`, `rule.apply`, `testPlan.create` |
| Secrets | `secret.create`, `secret.update`, `secret.delete` (names only, never values) |
| Access | `token.create`, `token.revoke`, `auth.login`, `auth.login_failed`, `auth.logout` |

Results and log lines sent by reporters are not audited one by one; the launch they belong to is.

## Example: a launch with two shards

```sh
api=https://fastproof.example.com/api/v1/projects/web
auth="Authorization: Bearer $FASTPROOF_TOKEN"

launch=$(curl -fsS -X POST "$api/launches" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"externalId":"pipeline-4711","name":"e2e","vcs":{"repository":"shop","branch":"main","commit":"3f2c1e0"}}' \
  | jq -r .id)

tar -czf shard1.tar.gz -C shard1 allure-results
curl -fsS -X POST "$api/launches/$launch/uploads?format=allure&fileName=shard1.tar.gz" \
  -H "$auth" -H 'Content-Type: application/gzip' --data-binary @shard1.tar.gz

curl -fsS -X POST "$api/launches/$launch/uploads?fileName=junit.xml" \
  -H "$auth" -H 'Content-Type: application/xml' --data-binary @shard2/junit.xml

curl -fsS -X POST "$api/launches/$launch/finish" -H "$auth" -H 'Content-Type: application/json' -d '{}'
```

Starting the launch again with the same `externalId` returns the existing launch (`200` instead of `201`),
so every shard can run the first call safely.
