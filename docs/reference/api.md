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
  credential was refused; `403` names the missing permission in `details.required`, also where a route allows
  an action only on your own items (for example a quick defect on a run item someone else executed).
- **Lists** return `{"items": [...], "next": "..."}`. Pass `next` as `before` to get the next page; `limit`
  defaults to 50, at most 500. The tokens list is the exception, see [Tokens](#tokens).
- **Limits** answer `413` with a message that names the limit and its unit, for example
  `the body exceeds max_upload_bytes (max 1073741824 bytes)`, and `details` `{"limit", "max"}`. Count limits
  of test management answer `409 limit_reached` with the same `details`. See [Limits](../operations/limits.md).
- **Concurrency:** test cases, plans and run items return `ETag: "r<revision>"`; `PUT` and `PATCH` of them need
  `If-Match` (see [Errors of test management](#errors-of-test-management)).

## Main resources

| Resource | Endpoints |
|---|---|
| Projects | `GET/POST /api/v1/projects`, `GET/PATCH/DELETE /api/v1/projects/{key}`, `GET/PUT .../settings` (see [Project settings](#project-settings)) |
| Launches | `POST /api/v1/projects/{key}/launches` (idempotent by `externalId`), `GET .../launches` with filters `branch`, `environment`, `status`, `attr=k:v`, `q`, `debug`; `GET/DELETE .../launches/{id}`, `POST .../launches/{id}/finish` |
| Uploads | `POST .../launches/{id}/uploads?format=auto\|allure\|junit\|ctrf\|playwright&fileName=` (raw body or multipart field `file`, answers `202`), `GET .../launches/{id}/uploads` |
| Streaming results | `POST .../launches/{id}/results` (FastProof JSON: one object, an array, or NDJSON); `POST/GET .../launches/{id}/logs`, `GET .../logs/stream` (see [Live logs](../guide/live-logs.md)) |
| Attachments | `POST .../launches/{id}/attachments`, `GET`/`HEAD /api/v1/projects/{key}/attachments/{id}/content` (byte ranges, see [below](#attachment-content-and-media-urls)), `POST .../attachments/{attachment}/media-url`, `GET`/`HEAD /api/v1/media/{token}` |
| Results | `GET .../launches/{id}/results` with `status`, `q`, `suite`, `label=k:v`, `flaky`, `fingerprint` (items list the `clientIds` of their attempts, the ids reporters send and live log lines use); `GET .../launches/{id}/tree?groupBy=suite\|feature\|package\|<labels>` (up to five label names; with `includeTests=true` the groups carry their tests as leaves, `testCount` and `durationMs`; groups that do not fit one response, about 5000 tests, are read page by page from `GET .../launches/{id}/tree/tests?path=<JSON array>`); the tree, its pages and the list take the explorer filters (`retried`, `muted`, `newFailures`, `severity`, `owner`, `tag`, `minDurationMs`, `maxDurationMs`, `defect`, `attachment`, `message`, `env`, see [Launch explorer](../guide/launch-explorer.md#api)); `GET .../launches/{id}/labels`, `.../facets`, `.../environments`; `GET /api/v1/projects/{key}/results/{id}` (with `muteUntil` and `muteReason` when muted) |
| Dashboards | `/dashboards`, widget data, preview, clone, lock (see [Dashboards](../guide/dashboards.md)) |
| Triage and analysis | defect types, `POST .../triage`, clusters, rules, flaky tests, compare, dashboard, test plans (see [Rules and triage](../guide/rules-and-triage.md)) |
| Integrations | `GET .../launches/{id}/gate`, project secrets, deliveries (see [Quality gates](../guide/quality-gates.md), [Notifications](../guide/notifications.md)) |
| History | `GET /api/v1/projects/{key}/test-cases/{testCaseId}/history?branch=&environment=` |
| Test cases | `GET/POST .../cases` (`POST` needs `name` and `body`), `GET/PUT/PATCH/DELETE .../cases/{n}`, `POST .../cases/{n}/review`, `.../cases/{n}/versions`, `.../history`, `.../identities`, `.../attachments`, `POST .../cases/bulk`, `GET .../cases/tree`, `.../case-folders`, `.../case-fields`, `GET .../test-identities`, `POST .../markdown/preview`, `POST .../cases/backfill` (see [Test cases](../guide/test-management.md)) |
| Test plans and manual runs | `GET/POST .../test-plans`, `GET/PATCH/DELETE .../test-plans/{id}`, `.../items`, `.../sync`, `GET/POST .../test-plans/{id}/launches` (`GET` lists the launches started from the plan), `.../testplan.json`; `GET/POST .../launches/{id}/run-items`, `GET .../run-items`, `GET/PATCH .../run-items/{itemId}`, `GET .../run-items/{itemId}/executions`, `PUT .../run-items/{itemId}/steps/{path}`, `POST .../run-items/{itemId}/rerun` (needs `If-Match`), `POST .../manual/close`, `.../manual/reopen`, `GET .../manual-report` (see [Test plans and manual runs](../guide/manual-runs.md)) |
| People | `GET /api/v1/projects/{key}/people?q=` (`projects:read`): up to 100 principals known to the project (users bound to it in the access file, active managed tokens whose role can execute runs there, and names used as assignees or executors in its runs and plans) as `{items: [{principal, display?}]}`, for assignee pickers |
| Imports and exports | `POST .../imports/preview`, `POST .../imports`, `GET/DELETE .../imports/{id}`, `POST .../imports/{id}/apply`, `GET .../cases/export`, `POST .../cases/exports` (see [Importing test cases](../guide/importing-test-cases.md)) |
| Live events | `GET /api/v1/projects/{key}/events` (Server-Sent Events: `launch.updated`, `upload.updated`; for test management `case.updated`, `case.archived`, `runItem.updated`, `manualRun.updated`, `tmJob.updated`) |
| Identity and tokens | `GET /api/v1/me`, `GET/POST /api/v1/tokens`, `GET /api/v1/tokens/options`, `GET /api/v1/tokens/self`, `GET/DELETE /api/v1/tokens/{id}` (see [Tokens](#tokens)) |
| Audit | `GET /api/v1/audit?action=&principal=&scope=&before=&limit=` (see [Audit log](#audit-log)) |
| Version | `GET /api/v1/version` (any signed-in principal): `{version}` of the server, the same as `fastproof version` |

## Attachment content and media URLs

`GET /api/v1/projects/{key}/attachments/{id}/content` serves the bytes of an attachment (the id of the
attachment, or of a reference to it, as results list it). It answers like a media server, so a video starts at
once and seeks anywhere:

- `Accept-Ranges: bytes`. One `Range` (`bytes=a-b`, `bytes=a-`, `bytes=-n`) is answered `206` with
  `Content-Range` and `Content-Length`; a range outside the content is `416` with
  `Content-Range: bytes */<size>`. A list of ranges is answered with the whole content (`200`). The server reads
  just the range from the store (an S3 range request, or a seek on the filesystem backend), never the bytes in front
  of it.
- `HEAD` returns the headers only.
- `ETag: "<sha256>"` (strong). `If-None-Match` with it answers `304`; `If-Range` with it keeps the range, any
  other value (or a date) sends the whole content.
- `Cache-Control: private, max-age=31536000, immutable`: the content behind an id never changes.
- `X-Content-Type-Options: nosniff`, `Content-Security-Policy: sandbox; default-src 'none'`,
  `Cross-Origin-Resource-Policy: same-origin` and `Content-Disposition` on every status. HTML, SVG and
  XML are always `attachment`; `?download=1` forces a download.

Elements such as `<video>` cannot send an `Authorization` header, so a client with a token asks for a media
URL:

```text
POST /api/v1/projects/{key}/attachments/{attachment}/media-url
-> 200 {"url": "/api/v1/media/<token>", "expiresAt": "2026-11-22T10:05:00.000Z"}
```

`{attachment}` is the attachment id (a UUID, as in the content route) or its SHA-256 (64 hex digits); the two
forms cannot be confused. The call needs the same permission as reading the content (`launches:read` in the
project) and answers `404` when the attachment is not in the project. `GET` and `HEAD` of the returned URL need
no `Authorization` header and no cookie and serve like the content route, ranges and validators included, with
`Cache-Control: private, max-age=<seconds until expiresAt>`; headers with credentials are ignored.

The token is opaque and URL-safe and carries the project, the SHA-256, the expiry and the principal that asked,
signed with HMAC-SHA256 under a key that combines a database secret with `FASTPROOF_MEDIA_URL_KEY`. A URL is
valid until `expiresAt`: `FASTPROOF_MEDIA_URL_TTL_SECONDS` after the request (300 by default, 3600 at most), or
the end of the caller's session or bearer token when that comes first. A URL stops working when the attachment
is deleted or when the principal that asked no longer reads the project (a revoked or expired API token, a
removed binding, a signed-out session); this is checked on every request and takes effect within a few seconds.
A URL that is malformed, expired, altered, or no longer allowed is `404` with the same body as an unknown path.
At most `FASTPROOF_MEDIA_MAX_STREAMS` bodies are streamed at once per replica; beyond that the answer is `503`
with `Retry-After`.

## Project settings

`GET /api/v1/projects/{key}/settings` returns the whole settings document of a project (needs
`projects:read`). `PUT` (needs `settings:write`) updates it by top-level area:

- each area present in the body replaces the stored area as a whole: `flaky`, `retention`, `rollup`, `gates`,
  `notifications`, `issueTrackers`, `fastmock`, `vcs`;
- `caseSync` ([code sync](../guide/test-management.md#settings)) is the exception: its keys are merged into the
  stored ones (`fields` by field), so `{"caseSync": {"branches": [...]}}` changes only the branches; `null`
  for a key returns it to its default and `"caseSync": null` resets the area;
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
| `role` | `viewer`, `tester`, `uploader`, `editor`, `admin` or a custom role |
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
| Test cases | `case.create`, `case.update`, `case.move`, `case.archive`, `case.restore`, `case.purge`, `case.bulk`, `case.linkIdentity`, `case.unlinkIdentity`, `case.review`, `case.backfill`, `caseFolder.create`, `caseFolder.update`, `caseFolder.delete`, `caseField.create`, `caseField.update`, `caseField.delete`, `caseSuggestion.accept`, `caseSuggestion.reject`, `caseSync.settings` |
| Plans and manual runs | `testPlan.create`, `testPlan.update`, `testPlan.items`, `testPlan.sync`, `testPlan.archive`, `run.start`, `runItem.update` (final status, defect, assignee), `runItem.rerun`, `manualRun.close`, `manualRun.reopen` |
| Imports and exports | `import.start`, `import.apply`, `import.cancel`, `export.create`, `export.download` (TestOps tokens are never recorded) |
| Secrets | `secret.create`, `secret.update`, `secret.delete` (names only, never values) |
| Access | `token.create`, `token.revoke`, `auth.login`, `auth.login_failed`, `auth.logout` |

Results and log lines sent by reporters are not audited one by one; the launch they belong to is. Writes of AI
agents through MCP carry `detail.source = "ai"` and `detail.agent`.

## Errors of test management

Besides the common `400`, `401`, `403` and `404` (an id of another project is always `404`), the test
management routes answer:

| Status | `code` | When |
|---|---|---|
| 400 | `invalid_case` | The case does not validate; `details` lists up to 50 `{path, message}`, `path` like `body.steps[2].expected` |
| 400 | `invalid_markdown` | A Markdown field is over its size; `details` `{path, max}` |
| 409 | `field_from_code` | A write to a value that code owns on an automated case; `details.fields` |
| 409 | `folder_not_empty` | Deleting a folder with subfolders or cases without `mode` |
| 409 | `folder_name_taken` | A sibling folder has the same name (case-insensitive) |
| 409 | `conflict` | A custom field key or source label is taken |
| 409 | `limit_reached` | A count limit; `details` `{limit, max}` |
| 409 | `case_has_runs` | Purging a case that was executed in a manual run |
| 409 | `case_number_taken` | An import asked for a case number that is used |
| 409 | `suggestion_stale` | Accepting an AI suggestion for a case that changed since |
| 409 | `suggestion_decided` | Accepting or rejecting a suggestion that is no longer pending |
| 409 | `plan_not_managed` | A managed-plan operation on a plan made from a launch |
| 409 | `plan_archived` | Changing or starting an archived plan |
| 409 | `plan_has_no_automated_cases` | `testplan.json` of a plan without automated cases |
| 409 | `launch_exists` | Starting a plan with the `externalId` of a launch that does not belong to it |
| 409 | `no_manual_run` | A manual run operation on a launch without run items |
| 409 | `manual_run_closed` | A write to an item of a closed manual run; reopen it first |
| 409 | `manual_run_open` | Reopening a run that is open |
| 409 | `run_item_not_final` | A defect on a run item without a final status |
| 409 | `no_issue_tracker` | A new-issue link when the project has no matching issue tracker |
| 403 | `ai_self_review` | The creator of an unreviewed AI case reviews or activates it, or the author of a suggestion accepts it |
| 409 | `launch_finished` | Uploads, results or a second finish into a finished launch by a caller that neither created it nor has `cases:sync` |
| 409 | `import_busy` | Another import of the project is running |
| 409 | `export_busy` | Too many zip exports of the caller (2) or the project (4) are queued or running; `details {limit, max}` |
| 412 | `precondition_failed` | `If-Match` names an older revision; `details` `{currentRevision, changedBy, source}` |
| 413 | `payload_too_large` | An attachment or import file over its limit |
| 400 | `bad_request` | The body is not valid JSON |
| 422 | `bad_request` | The body is valid JSON but has unknown fields, misses a required field or has a value of the wrong type |
| 428 | `precondition_required` | `PUT` or `PATCH` of a case, plan or run item without `If-Match` |

Unknown or archived case numbers in a plan or run item request answer `400` with `details.unknownCases`.

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
