# Notifications

When a launch finishes, FastProof can tell people and tools about it: a signed webhook, a Slack message, an
email, a Telegram message, and a comment and commit status on the merge request. Channels are project
settings, edited on the project's **Notifications** page and **Settings** page or through
`PUT /api/v1/projects/{key}/settings` (needs `settings:write`).

Every outbound call goes only to hosts on the server's allowlist, `FASTPROOF_EGRESS_ALLOW`, which is empty by
default; email needs `FASTPROOF_SMTP_URL`. See [Configuration](../operations/configuration.md#integrations).

## Events

| Event | When |
|---|---|
| `launchFinished` | Every finished launch (debug launches excluded) |
| `gateFailed` | The [quality gate](quality-gates.md) of the launch failed |
| `newFailures` | The launch has failures that the previous launch of the branch did not have |

Messages are sent once the gate is evaluated, so they always carry the gate result, the counts and the new
failures.

## Channels

```json
{
  "notifications": [
    {"id": "ci-hook", "kind": "webhook", "on": ["launchFinished"],
     "target": "https://hooks.example.com/fastproof", "secretRef": "ci-hook-key"},
    {"id": "team-slack", "kind": "slack", "on": ["gateFailed", "newFailures"],
     "secretRef": "slack-webhook", "filter": {"branches": ["main", "release/*"]}},
    {"id": "qa-mail", "kind": "email", "on": ["gateFailed"],
     "target": "qa@example.com, lead@example.com"},
    {"id": "nightly-tg", "kind": "telegram", "on": ["launchFinished"],
     "target": "-1001234567890", "secretRef": "telegram-bot", "filter": {"environments": ["nightly"]}}
  ]
}
```

| Field | Meaning |
|---|---|
| `id` | Unique id, `^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$` |
| `kind` | `webhook`, `slack`, `email` or `telegram` |
| `on` | One or more events |
| `target` | Webhook URL, Slack webhook URL (better kept in a secret), comma-separated email addresses (at most 20), or a Telegram chat id or `@channel` |
| `secretRef` | Name of a [project secret](#project-secrets): the HMAC key of a webhook, the Slack webhook URL, or the Telegram bot token (required for Telegram) |
| `filter` | Optional `branches` and `environments` globs; a launch must match both |

A project has at most 20 notifications.

### Webhook

A `POST` with `Content-Type: application/json` and these headers:

| Header | Value |
|---|---|
| `X-FastProof-Event` | The first event of the delivery |
| `X-FastProof-Delivery` | Delivery id; the same on every retry |
| `X-FastProof-Signature` | `sha256=<hex>`, HMAC-SHA256 of the raw body with the secret named by `secretRef` |

The body holds `project`, `launch` (id, number, name, status, branch, environment, repository, commit,
merge request, CI URL, launch page URL when `FASTPROOF_PUBLIC_URL` is set, duration and counts), `gate` (as
returned by the gate API), `newFailures` (`count`, up to 10 `names`, `baselineLaunchId`), `events`, `event`
and `deliveryId`. Verify the signature over the raw bytes before parsing, with a constant-time comparison:

```python
import hashlib, hmac

def verify(secret: bytes, body: bytes, header: str) -> bool:
    expected = "sha256=" + hmac.new(secret, body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, header)
```

### Slack, email and Telegram

These channels get a short text: project, launch and outcome, branch and environment, counts, the rules that
did not hold, new failures and the link to the launch. Slack uses an incoming webhook; email goes through the
server's SMTP relay (`FASTPROOF_SMTP_*`); Telegram uses the Bot API (`FASTPROOF_TELEGRAM_API_URL` for a proxy).

## Merge request comments and commit statuses

```json
{
  "vcs": {
    "provider": "gitlab",
    "url": "https://gitlab.example.com",
    "tokenSecretRef": "gitlab-token",
    "comment": true,
    "status": true
  }
}
```

| Field | Meaning |
|---|---|
| `provider` | `gitlab` or `github` |
| `url` | GitLab base URL, or the GitHub API URL (`https://api.github.com`, `https://github.example.com/api/v3`) |
| `tokenSecretRef` | Project secret with the token: a GitLab token with the `api` scope, or a GitHub token that may write pull request comments and commit statuses |
| `comment` | Post one comment per launch name on the merge request and update it in place on the next run (default `true`) |
| `status` | Set a commit status `fastproof/<project>/<launch name>` (default `true`) |

The launch must carry the repository URL (its host must match `url`), and a merge request number for comments
or a commit for statuses. The CLI and the reporters take them from GitLab CI, GitHub Actions and Jenkins
automatically. The status is `success`, `failed`/`failure` when the gate failed, and `running`/`pending` while
it is pending.

## Issue trackers

Triage can link an issue by key alone when the project knows the tracker:

```json
{
  "issueTrackers": [
    {"type": "jira", "urlTemplate": "https://jira.example.com/browse/{key}", "keyRegex": "[A-Z][A-Z0-9]+-[0-9]+"}
  ]
}
```

The first tracker whose `keyRegex` matches the whole key builds the URL. `type` is `jira`, `gitlab` or
`github`; a project has at most 10 trackers.

## Project secrets

Tokens, webhook keys and URLs that grant access are stored as project secrets, encrypted at rest with the
server key (`FASTPROOF_SECRETS_KEY_FILE`), and referenced by name from the settings. Values are write-only:
the API lists names and dates, never values.

```http
PUT    /api/v1/projects/{key}/secrets/{name}     {"value": "..."}
GET    /api/v1/projects/{key}/secrets
DELETE /api/v1/projects/{key}/secrets/{name}
```

Names match `^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$`, values have at most 8 KiB, and every call needs
`settings:write` and is audited. The **Settings** page of a project has a write-only form for them.

## Deliveries and retries

Each message is a delivery with its own retries: a `2xx` answer delivers it, a timeout, `408`, `429` or `5xx`
retries it with exponential backoff (`FASTPROOF_DELIVERY_BACKOFF_SECONDS`, doubled per attempt, at most an
hour) up to `FASTPROOF_DELIVERY_MAX_ATTEMPTS`, and any other answer or a host outside the allowlist fails it
at once. Deliveries are kept for 30 days.

```http
GET /api/v1/projects/{key}/notifications/deliveries?status=failed&limit=50
```

lists them with `status` (`pending`, `sending`, `delivered`, `failed`), attempts, the last error and the HTTP
status; the **Notifications** page shows the same list. Errors never contain the target URL, which may hold a
token.
