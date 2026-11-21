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
  defaults to 50, at most 500.

## Main resources

| Resource | Endpoints |
|---|---|
| Projects | `GET/POST /api/v1/projects`, `GET/PATCH/DELETE /api/v1/projects/{key}` |
| Launches | `POST /api/v1/projects/{key}/launches` (idempotent by `externalId`), `GET .../launches` with filters `branch`, `environment`, `status`, `attr=k:v`, `q`, `debug`; `GET/DELETE .../launches/{id}`, `POST .../launches/{id}/finish` |
| Uploads | `POST .../launches/{id}/uploads?format=auto\|allure\|junit\|ctrf\|playwright&fileName=` (raw body or multipart field `file`, answers `202`), `GET .../launches/{id}/uploads` |
| Streaming results | `POST .../launches/{id}/results` (FastProof JSON: one object, an array, or NDJSON) |
| Attachments | `POST .../launches/{id}/attachments`, `GET /api/v1/projects/{key}/attachments/{id}/content` |
| Results | `GET .../launches/{id}/results` with `status`, `q`, `suite`, `label=k:v`, `flaky`, `fingerprint`; `GET .../launches/{id}/tree?groupBy=suite\|feature\|package`; `GET /api/v1/projects/{key}/results/{id}` |
| History | `GET /api/v1/projects/{key}/test-cases/{testCaseId}/history?branch=&environment=` |
| Live events | `GET /api/v1/projects/{key}/events` (Server-Sent Events: `launch.updated`, `upload.updated`) |
| Identity and tokens | `GET /api/v1/me`, `/api/v1/tokens` |
| Audit | `GET /api/v1/audit` |

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
