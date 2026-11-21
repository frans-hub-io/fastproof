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
| `results:triage` | Set defect types, comments, issue links and mutes |
| `rules:write` | Manage triage rules |
| `settings:write` | Change project and server settings |
| `audit:read` | Read the audit log |
| `tokens:read`, `tokens:write` | List and manage API tokens |

Built-in roles:

| Role | Permissions |
|---|---|
| `viewer` | `projects:read`, `launches:read` |
| `uploader` | `projects:read`, `launches:read`, `launches:write` |
| `editor` | everything of `uploader`, plus `results:triage`, `rules:write` |
| `admin` | `*` (every permission) |

A **binding** gives a role to a user, an OIDC group or a token, scoped globally, to a list of projects, or to
all projects (`*`, including projects created later). Project-scoped bindings never grant global-only
permissions such as `projects:write`.

CI tokens should have the `uploader` role on the projects they report to, and nothing more.

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
  `FASTPROOF_SECRETS_KEY_FILE`, bound to their project and name, and write-only through the API. Rotate the key
  as described in [Secrets key rotation](configuration.md#secrets-key-rotation).
- Integrations reach only the hosts in `FASTPROOF_EGRESS_ALLOW` (empty by default: no outbound calls). Names
  matched only by a wildcard must resolve to public addresses, every redirect is checked again, and proxies
  from the environment are ignored.
- Webhooks are signed with HMAC-SHA256 (`X-FastProof-Signature`); verify the signature on the receiving side.
- Keep port 9090 (metrics, health, readiness) inside the cluster; the Helm chart's NetworkPolicy allows it
  only from the monitoring namespace.

## Audit

Every change (projects, launches, triage, rules, settings, tokens, sign-ins) is recorded with who, what, when
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
