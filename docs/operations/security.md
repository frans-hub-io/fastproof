# Security

FastProof is secure by default: every API and UI route needs authentication, except health, readiness and
metrics on the separate operations port and the sign-in flow itself.

## Authentication

| Method | Used by | Notes |
|---|---|---|
| Static tokens | Bootstrap, automation | `FASTPROOF_ADMIN_TOKENS_FILE`, one `name:role:sha256:<hex>` line per token; the role is granted globally. Prefer hashes: then the file holds no secret |
| Managed API tokens | CI, scripts, integrations | Created in the UI or with `POST /api/v1/tokens`; prefix `fp_`, mandatory expiry, scoped to projects, only the SHA-256 is stored, revocable; a token can only grant what its creator holds |
| API token sign-in | People in the UI without single sign-on | The token is pasted on the sign-in page, kept in session storage of that tab and sent as `Authorization: Bearer` to the API only (never in URLs); bearer requests need no CSRF header. Without OIDC, `/auth/login` redirects to that page; `GET /auth/config` tells the page whether SSO is configured |
| OIDC sign-in | People in the UI | Authorization code flow with PKCE, server-side sessions, `__Host-` cookies (`HttpOnly`, `Secure`, `SameSite=Lax`), logout at the provider |
| OIDC bearer JWT | Services with tokens from your identity provider | Enabled with `FASTPROOF_OIDC_API_AUDIENCE`; `iss`, `aud`, `exp`, `nbf` and the algorithm are checked |

Tokens are sent as `Authorization: Bearer <token>`. Unknown, expired and revoked credentials all get the same
`401` answer. Browser sessions must send `X-FransHub-Request: 1` on requests that change something, and
requests from a foreign `Origin` are refused.

## Roles and permissions

Permissions are `resource:action` strings:

| Permission | Allows |
|---|---|
| `projects:read` | See the project and its settings |
| `projects:write` | Create and delete projects (global only) |
| `launches:read` | See launches, results, attachments and history |
| `launches:write` | Create launches, upload reports, finish and delete launches |
| `launches:rerun` | Rerun tests of a launch in the project's CI ([Rerun](../guide/rerun.md)); starts pipelines with the stored CI token, so the `uploader` role (CI report tokens) does not have it |
| `results:triage` | Set defect types, comments and issue links |
| `results:mute` | Mute and unmute results (quality gates ignore muted failures); also needs `results:triage` |
| `rules:write` | Manage triage rules |
| `settings:write` | Change project and server settings |
| `audit:read` | Read the audit log |
| `cases:read` | See test cases, folders, fields, plans and their history |
| `cases:write` | Create, change and archive test cases and folders |
| `cases:import` | Import test cases from files and other tools |
| `cases:sync` | Let results uploaded with this permission into launches created with it update test cases from code ([code sync](../guide/test-management.md#code-sync)) |
| `plans:write` | Create, change, sync and archive test plans; reopen closed manual runs |
| `runs:execute` | Start launches from test plans and execute manual runs |
| `ai:propose` | Create work for people to review through MCP: AI draft cases, case suggestions, disabled triage rules ([agent tokens](../guide/mcp-copilot.md#agent-tokens)) |
| `tokens:read`, `tokens:write` | List and manage API tokens |

Built-in roles:

| Role | Permissions |
|---|---|
| `viewer` | `projects:read`, `launches:read`, `cases:read` |
| `tester` | `projects:read`, `launches:read`, `cases:read`, `runs:execute` |
| `uploader` | `projects:read`, `launches:read`, `launches:write`, `cases:read`, `cases:sync` |
| `editor` | everything of `uploader`, plus `launches:rerun`, `results:triage`, `results:mute`, `rules:write`, `cases:write`, `cases:import`, `plans:write`, `runs:execute` |
| `agent` | `projects:read`, `launches:read`, `cases:read`, `results:triage`, `ai:propose` (tokens of AI agents; no mutes, no direct changes to cases; their triage is recorded as `source: ai`) |
| `admin` | `*` (every permission) |

A **binding** gives a role to a user, an OIDC group or a token, scoped globally, to a list of projects, or to
all projects (`*`, including projects created later). Project-scoped bindings never grant global-only
permissions such as `projects:write`.

CI tokens should have the `uploader` role on the projects they report to, and nothing more. Its `cases:sync`
lets launches of trusted branches update test cases from code; give pipelines of merge requests and forks a
custom role without it (see [Trusted launches](../guide/test-management.md#trusted-launches)). Custom field
definitions, the code sync settings, purging cases and starting the backfill of cases need `settings:write`.

### Access file

Custom roles and bindings are read from the YAML file named by `FASTPROOF_ACCESS_FILE` (the Helm chart
renders it from `auth.access`):

```yaml
roles:
  - name: release-manager
    permissions: ["launches:*", "results:triage"]
bindings:
  - subject: group:qa
    role: editor
    scopes: [web, mobile]
  - subject: group:platform
    role: admin
    global: true
  - subject: user:alice
    role: viewer
    scopes: ["*"]
```

Errors in the file stop the server at start with a message naming the entry.

## Attachments are data

Uploaded reports are never executed on the server, and attachments are served so that they cannot run in the
FastProof origin:

- Only principals that can read the project can fetch its attachments.
- Every attachment response carries `X-Content-Type-Options: nosniff` and
  `Content-Security-Policy: sandbox; default-src 'none'`.
- HTML, SVG and XML attachments are always served as downloads (`Content-Disposition: attachment`), as is
  any attachment requested with `?download=1`.
- HTML descriptions from reports are sanitized before they are stored.
- Masked and hidden test parameters are used for test identity only and never returned.
- Test case texts are stored as Markdown and rendered on the server: raw HTML becomes text, links work only for
  `http`, `https` and `mailto`, and images only for attachments of the same project.
- Manual run attachments and case attachments are served by the same rules as launch attachments.
- Content is immutable and addressed by its SHA-256, so it is served with a strong `ETag` and
  `Cache-Control: private, max-age=31536000, immutable`; every status (200, 206, 304, 416) keeps the
  sandboxing headers.
- A [media URL](../reference/api.md#attachment-content-and-media-urls) lets a `<video>` or `<img>` element
  load one attachment without an `Authorization` header or cookie. It is signed (HMAC-SHA256 under a key
  derived from a database secret and a secret held outside the database), names one attachment of one project
  and is valid until its `expiresAt`: 5 minutes by default, one hour at most, and never beyond the session or
  bearer token of the caller. Each request checks that the attachment exists and that the principal that asked
  still reads the project. Every refusal is the same `404`. Media responses are cacheable only until
  `expiresAt`, and at most `FASTPROOF_MEDIA_MAX_STREAMS` bodies stream at once per replica.
- The token is removed from the request path before FastProof logs or traces it, and metrics label the route as
  `/api/v1/media/{token}`. The token is part of the URL path, so reverse proxies and ingress controllers in front
  of FastProof record it in their access logs for as long as the URL is valid; give those logs the same
  handling as other credentials.

## Limits

Every parser has size, depth and count limits, and one byte budget per upload covers all expansions. See
[Limits](limits.md).

## Transport and secrets

- Terminate TLS at your gateway or ingress, or serve it directly with `FASTPROOF_TLS_CERT_FILE` and
  `FASTPROOF_TLS_KEY_FILE` (optionally requiring client certificates).
- Connect to PostgreSQL with `sslmode=verify-full` and `FASTPROOF_DATABASE_CA_FILE`; to S3 over HTTPS with
  `FASTPROOF_S3_CA_FILE` for a private CA.
- Pass secrets as files (`*_FILE` variables). The Helm chart mounts every secret as a file, never as an
  environment variable, except the optional Valkey URL.
- Secrets are redacted from logs and never returned by the API.
- Project secrets (integration tokens, webhook keys) are encrypted at rest with AES-256-GCM under the key in
  `FASTPROOF_SECRETS_KEY_FILE`, bound to their project, name, purpose and destination origin, and write-only
  through the API. A secret is used only for its purpose and sent only to its origin, so pointing a setting at
  another URL needs the secret written again ([Project secrets](../guide/notifications.md#project-secrets)).
  Rotate the key as described in [Secrets key rotation](configuration.md#secrets-key-rotation).
- Integrations reach only the hosts in `FASTPROOF_EGRESS_ALLOW` (empty by default: no outbound calls). An
  entry with a port allows only that port. Names matched only by a wildcard must resolve to public addresses,
  every redirect is checked again and loses credential headers when it leaves the origin, calls that carry a
  token (GitLab, GitHub, FastMock) or a credential URL (Microsoft Teams workflows) do not follow redirects, and
  proxies from the environment are ignored.
- Merge request comments and commit statuses go only to the repository pinned in the project settings, and text
  from launches is escaped in them.
- Webhooks are signed with HMAC-SHA256 over a timestamp and the body (`X-FastProof-Timestamp`,
  `X-FastProof-Signature`); verify the signature, the timestamp and the delivery id on the receiving side
  ([Webhook](../guide/notifications.md#webhook)).
- Notification targets and integration URLs in the project settings are shown only to callers with
  `settings:write`; changes of those URLs are audited with the old and the new value. URLs that are
  credentials (Slack incoming webhooks, Microsoft Teams workflows) belong in project secrets; Teams accepts
  nothing else.
- Keep port 9090 (metrics, health, readiness) inside the cluster; the Helm chart's NetworkPolicy allows it
  only from the monitoring namespace.

## Audit

Every change (projects, launches, triage, rules, settings, tokens, sign-ins, test cases, plans, manual runs,
imports and exports) is recorded with who, what, when
and the project. Admins read the audit log in the UI or with `GET /api/v1/audit`.

## Container image

The image `franshub/fastproof` is built on `gcr.io/distroless/static-debian13:nonroot`: one static binary,
CA certificates and time zone data, no shell and no package manager. It runs as UID 65532 and works with a
read-only root filesystem, all capabilities dropped and the `RuntimeDefault` seccomp profile, which is what
the Helm chart sets.

## Reporting a vulnerability

Please report vulnerabilities privately through
[GitHub private vulnerability reporting](https://github.com/frans-hub-io/fastproof/security/advisories/new),
not in public issues. See [SECURITY.md](https://github.com/frans-hub-io/fastproof/blob/main/SECURITY.md) for
what to include and what to expect.
