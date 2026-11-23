# Notifications

When a launch finishes, FastProof can tell people and tools about it: a signed webhook, a Slack message, a
Microsoft Teams card, an email, a Telegram message, and a comment and commit status on the merge request. Channels are project
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
| `manualRunClosed` | The [manual run](manual-runs.md#combined-launches-and-closing-a-run) of the launch was closed and the gate evaluated again with its results |

Closing the manual run of a launch that CI has already finished evaluates its gate again and sends only
`manualRunClosed`, with the new gate result and counts; `launchFinished`, `gateFailed` and `newFailures` are
not sent a second time for the same launch. Subscribe a channel to `manualRunClosed` to hear about closed
manual runs and a gate that fails only after them. A purely manual launch finishes when its run is closed and
sends `launchFinished` (and `gateFailed` when it applies) together with `manualRunClosed`.

Messages are sent once the gate is evaluated, so they always carry the gate result, the counts and the new
failures.

## Channels

```json
{
  "notifications": [
    {"id": "ci-hook", "kind": "webhook", "on": ["launchFinished"],
     "target": "https://hooks.example.com/fastproof", "secretRef": "ci-hook-key"},
    {"id": "team-slack", "kind": "slack", "on": ["gateFailed", "newFailures"],
     "targetSecretRef": "slack-webhook", "filter": {"branches": ["main", "release/*"]}},
    {"id": "qa-mail", "kind": "email", "on": ["gateFailed"],
     "target": "qa@example.com, lead@example.com"},
    {"id": "nightly-tg", "kind": "telegram", "on": ["launchFinished"],
     "target": "-1001234567890", "secretRef": "telegram-bot", "filter": {"environments": ["nightly"]}},
    {"id": "nightly-teams", "kind": "teams", "on": ["gateFailed", "newFailures"],
     "targetSecretRef": "teams-qa-channel",
     "filter": {"launchNames": ["nightly-*"], "attributes": ["trigger=schedule"]}}
  ]
}
```

| Field | Meaning |
|---|---|
| `id` | Unique id, `^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$` |
| `kind` | `webhook`, `slack`, `teams`, `email` or `telegram` |
| `on` | One or more events |
| `target` | Webhook URL, Slack webhook URL, comma-separated email addresses (at most 20), or a Telegram chat id or `@channel` |
| `targetSecretRef` | Webhook and Slack: a [project secret](#project-secrets) holding the URL, instead of `target`. Use it for every URL that is itself a credential, such as a Slack incoming webhook URL. Teams: required, the secret with purpose `teams` holding the workflow URL (Teams takes no `target`) |
| `secretRef` | Webhook: the [project secret](#project-secrets) with the HMAC signing key; Telegram: the secret with the bot token (required) |
| `filter` | Optional routing: `branches`, `environments` and `launchNames` globs and `attributes` (see below) |

A project has at most 20 notifications. Callers that can read the project but lack `settings:write` (viewers,
CI tokens) get the settings with every notification `target` and the `vcs`, `fastmock` and `rerun` URLs emptied and
`"redacted": true` on each.

### Routing with filters

Every notification of a project sees every launch of the project; `filter` narrows that down, so one project
can send its nightly failures to one channel and its merge request runs to another:

| `filter` field | A launch matches when |
|---|---|
| `branches` | Its branch matches one of the globs |
| `environments` | Its environment matches one of the globs |
| `launchNames` | Its name matches one of the globs, for example `nightly-*` |
| `attributes` | It has every listed attribute: each entry is `key=value`, the value may be a glob (`suite=e2e-*`), and `key=*` only asks for the key |

`*` in a glob matches any text, everything else matches literally. Every field that is given must match; an
absent or empty field matches everything. Each list holds at most 50 entries of 1 to 256 bytes. Launch
attributes come from the reporter or the CLI (`--attr trigger=schedule`) and from
`POST .../launches` (`"attributes": [{"key": "trigger", "value": "schedule"}]`). For example:

```json
{
  "notifications": [
    {"id": "nightly", "kind": "teams", "on": ["gateFailed", "newFailures"],
     "targetSecretRef": "teams-qa-nightly",
     "filter": {"branches": ["main"], "launchNames": ["nightly-*"], "attributes": ["trigger=schedule"]}},
    {"id": "merge-requests", "kind": "teams", "on": ["gateFailed"],
     "targetSecretRef": "teams-dev-reviews",
     "filter": {"branches": ["feature/*", "fix/*"], "attributes": ["mr=*"]}}
  ]
}
```

### Webhook

A `POST` with `Content-Type: application/json` and these headers:

| Header | Value |
|---|---|
| `X-FastProof-Event` | The first event of the delivery |
| `X-FastProof-Delivery` | Delivery id; the same on every retry |
| `X-FastProof-Timestamp` | Unix time in seconds when this attempt was signed |
| `X-FastProof-Signature` | `sha256=<hex>`, HMAC-SHA256 of `<timestamp>.<raw body>` with the secret named by `secretRef` |

The body holds `project`, `launch` (id, number, name, status, branch, environment, repository, commit,
merge request, CI URL, launch page URL when `FASTPROOF_PUBLIC_URL` is set, duration and counts), `gate` (as
returned by the gate API), `passRate` (percent of passed among executed results, muted ones excluded, as the
gates measure it), `newFailures` (`count`, up to 10 `names`, the same failures as `items` with `name`,
`resultId` and the result page `url`, and `baselineLaunchId`), `events`, `event` and `deliveryId`. A
[test notification](#test-notifications) also has `"test": true`. The body is the same on every retry; each attempt is signed again with its own timestamp.

To accept a webhook, check the signature over the timestamp, a dot and the raw bytes before parsing, with a
constant-time comparison; reject timestamps more than five minutes away from your clock; and drop delivery ids
you have already processed:

```python
import hashlib, hmac, time

def verify(secret: bytes, body: bytes, timestamp: str, signature: str, tolerance: int = 300) -> bool:
    if not timestamp.isdigit() or abs(time.time() - int(timestamp)) > tolerance:
        return False
    signed = timestamp.encode() + b"." + body
    expected = "sha256=" + hmac.new(secret, signed, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, signature)
```

### Microsoft Teams (Workflows)

Teams messages go through a Teams workflow (Power Automate) whose trigger is **When a Teams webhook request is
received**; Office 365 connectors and their incoming webhooks are retired. FastProof posts an Adaptive Card
(version 1.5) with the project, the launch name and number linked to its page, the branch, the environment,
the launch status, the counts (passed, failed, broken, skipped, flaky), the pass rate, the duration, the quality
gate result with the rules that did not hold, and up to 10 new failures, each linked to its result. The
**Open launch** and **Open failures** buttons open the launch page. Links and buttons need
`FASTPROOF_PUBLIC_URL`; without it the card has no links.

1. In Teams, open the channel that should get the cards, choose **More options (…)** › **Workflows**, and pick
    the template **Post to a channel when a webhook request is received** (some tenants list it as **Send
    webhook alerts to a channel**). Name the workflow, confirm the team and the channel, and save it. To post
    to a chat instead, start from **Post to a chat when a webhook request is received**.

2. Copy the URL the workflow shows when it is saved (later: open the workflow in the Workflows app or in Power
    Automate, open its trigger card and copy the URL shown there). It looks like
    `https://<environment>.<nn>.environment.api.powerplatform.com:443/powerautomate/automations/direct/workflows/<id>/triggers/manual/paths/invoke?api-version=1&sp=…&sv=1.0&sig=…`.
    The `sig` parameter is the credential: anyone with the URL can post to the channel. Leave the trigger's
    **Who can trigger the flow** at **Anyone**; FastProof does not send Microsoft Entra tokens, and a trigger
    limited to tenant users answers `401`.

3. Store the URL as a project secret with purpose `teams` (needs `settings:write`). The secret is bound to the
    exact origin of the URL, taken from the value; `origin` may be omitted. The URL must use `https`:

    ```bash
    curl -X PUT "https://fastproof.example.com/api/v1/projects/shop/secrets/teams-qa-channel" \
      -H "Authorization: Bearer $FASTPROOF_TOKEN" -H "Content-Type: application/json" \
      --data-binary @- <<'JSON'
    {"value": "<the workflow URL>", "purpose": "teams"}
    JSON
    ```

    The **Settings** › **Secrets** page of the project does the same (purpose **Microsoft Teams workflow URL**).

4. Add the notification on the **Notifications** page (kind **Microsoft Teams**, the secret, the events and
    the [filters](#routing-with-filters)) or in the settings:

    ```json
    {"id": "qa-teams", "kind": "teams", "on": ["gateFailed", "newFailures"], "targetSecretRef": "teams-qa-channel"}
    ```

    A Teams notification takes only `targetSecretRef`: a plain `target` or a secret of another purpose is
    refused with `400`.

5. Allow the workflow host in `FASTPROOF_EGRESS_ALLOW` on every replica with the `scheduler` role (they send
    the deliveries) and on the replicas that serve the API (they send test notifications). Workflow URLs are on
    `*.environment.api.powerplatform.com` (port 443); list the exact host of your URL, or the wildcard for
    several environments:

    ```bash
    FASTPROOF_EGRESS_ALLOW=*.environment.api.powerplatform.com:443
    ```

    URLs of Power Automate flows on `*.logic.azure.com` stopped working on 30 November 2025; copy the new URL
    from the flow and write the secret again. Workflows built in Azure Logic Apps still use
    `*.logic.azure.com`: add `*.logic.azure.com:443` only for those.

6. Press **Send test notification** on the card of the notification (or call the
    [test API](#test-notifications)); the channel gets a sample card marked **Test notification**.

The card text that comes from launches (project, launch and test names, branches, environments, gate names) is
put into plain-text runs only, never into Markdown, with control characters removed and link syntax and
`{{…}}` date functions broken, so it cannot add links, mentions or formatting. The message stays below
24 KiB (Teams refuses messages over 28 KB): the lists of failures and rules are shortened first, then long
texts, and the card says how many items it left out. The workflow answers `202 Accepted`; a `429` (Teams
allows a few requests per second per workflow) or `5xx` is retried, a `401`/`403` (the trigger is not open to
**Anyone**) or `404` (the workflow was deleted, turned off or has a new URL) fails at once, and redirects are
not followed.

A workflow belongs to the person who created it: when that account leaves, the workflow stops. Add co-owners
to the workflow in Power Automate for channels that must keep working.

### Slack, email and Telegram

These channels get a short text: project, launch and outcome, branch and environment, counts, the rules that
did not hold, new failures and the link to the launch. Slack uses an incoming webhook (keep its URL in a
secret with purpose `slack` and refer to it with `targetSecretRef`); email goes through the server's SMTP relay
(`FASTPROOF_SMTP_*`); Telegram uses the Bot API (`FASTPROOF_TELEGRAM_API_URL` for a proxy).

## Merge request comments and commit statuses

```json
{
  "vcs": {
    "provider": "gitlab",
    "url": "https://gitlab.example.com",
    "repository": "shop/web-app",
    "tokenSecretRef": "gitlab-token",
    "sourceBranches": ["feature/*", "fix/*"],
    "comment": true,
    "status": true
  }
}
```

| Field | Meaning |
|---|---|
| `provider` | `gitlab` or `github` |
| `url` | GitLab base URL, or the GitHub API URL (`https://api.github.com`, `https://github.example.com/api/v3`) |
| `repository` | The only repository FastProof posts to: the project path on GitLab (`group/subgroup/app`), `owner/repo` on GitHub |
| `tokenSecretRef` | Project secret with purpose `vcs` and the origin of `url`, holding the token: a GitLab token with the `api` scope, or a GitHub token that may write pull request comments and commit statuses |
| `sourceBranches` | Optional branch globs: only launches of these branches (the merge request source branch) post. Absent: every branch |
| `comment` | Post one comment per launch name on the merge request and update it in place on the next run (default `true`) |
| `status` | Set a commit status `fastproof/<project>/<launch name>` (default `true`) |

The launch must carry the repository URL, and a merge request number for comments or a commit for statuses.
The CLI and the reporters take them from GitLab CI, GitHub Actions and Jenkins automatically. Launch metadata
comes from whoever uploads results, so FastProof posts only when the launch repository is `repository` on the
host of `url`; a launch that names another repository gets a failed delivery ("is not vcs.repository") and
no API call. The status is `success`, `failed`/`failure` when the gate failed, and `running`/`pending` while
it is pending.

The token is sent as `Authorization: Bearer` (GitLab accepts its access tokens that way) and API calls do not
follow redirects. Text taken from launches (launch and test names) is escaped in the comment, so it cannot
add links, images, mentions, references or HTML.

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
the API lists names, purposes, origins and dates, never values.

```http
PUT    /api/v1/projects/{key}/secrets/{name}     {"value": "...", "purpose": "vcs", "origin": "https://gitlab.example.com"}
GET    /api/v1/projects/{key}/secrets
DELETE /api/v1/projects/{key}/secrets/{name}
```

Each secret is bound, when it is written, to one purpose and optionally to the one origin
(`scheme://host[:port]`) it may be sent to:

| `purpose` | Used by | `origin` |
|---|---|---|
| `vcs` | `vcs.tokenSecretRef` | Required: the origin of `vcs.url` |
| `fastmock` | `fastmock.tokenSecretRef` | Required: the origin of `fastmock.url` |
| `ci` | `rerun.tokenSecretRef` | Required: the origin of `rerun.url` (see [Rerun](rerun.md)) |
| `slack` | `targetSecretRef` of a Slack notification | Optional; when given, the URL in the value must be on it |
| `teams` | `targetSecretRef` of a Teams notification | Taken from the workflow URL in the value (`https`); when given, it must be that origin |
| `webhook` | `secretRef` (signing key) or `targetSecretRef` (URL) of a webhook | Optional; when given, the webhook URL must be on it |
| `telegram` | `secretRef` of a Telegram notification | Optional; when given, `FASTPROOF_TELEGRAM_API_URL` must be on it |
| `smtp` | Reserved for project mail credentials | Optional |

A secret is never used for another purpose or sent to another origin. Saving settings that refer to a secret
of another purpose, or to a URL outside the secret's origin, is refused with `400`; a delivery or FastMock
query that meets such a reference (or a secret that does not exist) fails with the same message and sends
nothing. To move an integration to a new URL, write its secret
again with the new origin, then save the settings (a Teams workflow that gets a new URL needs its secret written
again with that URL). Every change of a URL in the settings (`vcs.url`, `rerun.url`,
`vcs.repository`, `fastmock.url`, webhook and Slack targets, issue tracker templates) is written to the audit
log as `project.settings.urls` with the old and the new value (user info and query strings left out).

Names match `^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$`, values have at most 8 KiB, and every call needs
`settings:write` and is audited. The **Settings** page of a project has a write-only form for them.

## Deliveries and retries

Each message is a delivery with its own retries: a `2xx` answer delivers it, a timeout, `408`, `429` or `5xx`
retries it with exponential backoff (`FASTPROOF_DELIVERY_BACKOFF_SECONDS`, doubled per attempt, at most an
hour) up to `FASTPROOF_DELIVERY_MAX_ATTEMPTS`, and any other answer or a host outside the allowlist fails it
at once. When a `429` or `503` carries `Retry-After` (seconds or an HTTP date), the next attempt waits at
least that long, at most an hour. Deliveries are kept for 30 days.

```http
GET /api/v1/projects/{key}/notifications/deliveries?status=failed&limit=50
```

lists them with `status` (`pending`, `sending`, `delivered`, `failed`), attempts, the last error and the HTTP
status; the **Notifications** page shows the same list. Errors never contain the target URL, which may hold a
token.

## Test notifications

```http
POST /api/v1/projects/{key}/notifications/{id}/test
```

sends a sample message to one saved notification of any kind: a made-up launch `Sample launch #0` of the
project with a failed gate, counts and two new failures, linking to the project page. Webhook bodies carry
`"test": true` and are signed as usual; Teams cards and texts say **Test notification**. The message is sent
once, without retries, and is not listed among the deliveries; the answer says how the receiver answered:

```json
{"delivered": false, "responseStatus": 404, "error": "HTTP 404 (the workflow was deleted, turned off or got a new URL)"}
```

It needs `settings:write` (viewers and CI tokens get `403`), answers `404` for an unknown notification id and
`429` when the same notification was tested less than 5 seconds ago, and is written to the audit log as
`notification.test`. Test the saved settings: save changes before testing them.
