# Issue trackers

FastProof creates bugs in Jira Cloud, Jira Server and Data Center, GitLab, GitHub and Azure DevOps Boards from
a failed result or a failure cluster, links existing issues, and reads their state back. The state shows next
to the defect of every linked result; when an issue is closed while its tests are still muted, FastProof
suggests unmuting them.

## Connect a tracker

A tracker in the project settings (`issueTrackers`, **Settings → Integrations**) turns issue keys into links.
Give it an `id` and `api` settings to create issues through its API:

```json
{
  "issueTrackers": [
    {
      "id": "jira",
      "type": "jira",
      "urlTemplate": "https://example.atlassian.net/browse/{key}",
      "keyRegex": "[A-Z][A-Z0-9]+-[0-9]+",
      "api": {
        "url": "https://example.atlassian.net",
        "tokenSecretRef": "jira-token",
        "project": "SHOP",
        "issueType": "Bug",
        "labels": ["fastproof"],
        "attachScreenshots": true
      }
    }
  ]
}
```

| Tracker | `type` | `api.url` | `api.project` | Token secret value | Description markup |
|---|---|---|---|---|---|
| Jira Cloud | `jira` | the site, `https://example.atlassian.net` | project key | `email:api-token` (basic) | Atlassian Document Format (REST API v3) |
| Jira Server, Data Center | `jira` with `"deployment": "server"` | the base URL | project key | a personal access token (bearer), or `user:password` with `"auth": "basic"` | wiki markup (REST API v2) |
| GitLab | `gitlab` | the base URL, `https://gitlab.example.com` | `group/app` or the project id | a token with the `api` scope | Markdown |
| GitHub | `github` | `https://api.github.com` or `https://github.example.com/api/v3` | `owner/repo` | a token that can write issues | Markdown |
| Azure DevOps | `azure` | the organization, `https://dev.azure.com/example` | the project name | a personal access token with Work Items read and write (basic) | HTML |

Store the token as a project secret with purpose `tracker` bound to the origin of `api.url`:

```http
PUT /api/v1/projects/SHOP/secrets/jira-token
{"value": "bot@example.com:ATATT...", "purpose": "tracker", "origin": "https://example.atlassian.net"}
```

The secret is sent only to that origin; changing `api.url` to another host needs the secret written again.
The token also stays with the tracker type, deployment, authentication, project and API URL it was configured
for: pointing it at another project (or giving it to another tracker entry) needs the secret written again
first, then the settings saved within an hour. Every change of these fields is audited as
`project.settings.trackers` with the old and the new values. Use the narrowest token the tracker offers: a
GitLab project access token, a GitHub fine-grained token for the one repository, a Jira account that can only
work in the project, an Azure DevOps PAT scoped to Work Items.
The host must be in `FASTPROOF_EGRESS_ALLOW`. Calls never follow redirects, and error messages never contain
the URL or the token. **Test the connection** in the settings (or
`POST /api/v1/projects/{key}/issue-trackers/{trackerId}/test`, `settings:write`) reads the configured project
with the saved settings.

Other `api` fields:

| Field | Default | Meaning |
|---|---|---|
| `auth` | see the table above | `basic` (the secret is `user:token`) or `bearer` |
| `issueType` | `Bug` | Jira issue type name, Azure DevOps work item type (a `Bug` gets the description in Repro Steps) |
| `labels` | none | Labels (tags in Azure DevOps) of new issues, at most 10 |
| `attachScreenshots` | `false` | Attach up to 3 images of the result's last attempt (PNG, JPEG, GIF, WebP, 5 MiB each). A request can turn it off, not on. GitHub has no API for issue attachments; GitLab gets them after the issue exists |
| `titleTemplate`, `bodyTemplate` | see below | Title (one line, 255 characters) and description of new issues |
| `sync` | `poll` | `poll`, `webhook` or `off` (see [State sync](#state-sync)) |
| `webhookSecretRef` | none | With `sync: webhook`: a secret with purpose `tracker-webhook` (at least 16 bytes) |
| `closedStates` | `Closed`, `Done`, `Removed` | Azure DevOps states that count as closed |

## Create an issue

**Triage** on a failed result or a failure cluster has **Create or link an issue in the tracker**. It shows a
preview of the title and description, takes an optional note, creates the issue, puts it on the defects of the
results and starts following its state. The API:

```http
POST /api/v1/projects/{key}/tracker-issues
{"tracker": "jira", "requestId": "0f1d…", "resultIds": ["…"], "note": "Seen after the deploy"}
```

or `"launchId"` and `"fingerprint"` instead of `resultIds` for every result of a failure cluster.
`requestId` (a UUID chosen by the client) makes the call idempotent: a retry answers the issue the first call
created (`"repeated": true`). `"dryRun": true` only renders the preview. Optional: `title`, `note` (plain text
above the description), `attachScreenshots: false`, `defectTypeId` (also set this defect type). The answer has the
issue, the preview, the number of attached screenshots and `updatedResults`.

Link an existing issue instead (FastProof reads it from the tracker, so it must exist, and it must belong to
the configured project: the Jira key prefix and project, the Azure DevOps team project, the GitLab project or
GitHub repository):

```http
POST /api/v1/projects/{key}/tracker-issues/link
{"tracker": "jira", "issueKey": "SHOP-123", "resultIds": ["…"]}
```

Both need `issues:write` and `results:triage` (the `editor` and `admin` roles; the `agent` role of AI agents has
no `issues:write`, so agents cannot act in trackers) and are audited (`tracker.issue.create`,
`tracker.issue.link`). A project makes at most 60 create attempts an hour (failed ones count), at most 120 links
an hour, and follows at most 10 000 issues; a request lists at most 100 results. Errors about settings, secrets
or the egress policy say only that the connection is not usable; **Test the connection** shows the detail to
settings writers.

### Templates

Templates are text with `{{placeholders}}`. The default title is `{{name}}: {{messageLine}}`; the default
description lists the test, status, launch, links back to the launch and the result, branch, environment, the
number of results with the failure, the error message, the trace and, for a cluster, the tests.

| Placeholder | Value |
|---|---|
| `name`, `fullName`, `status` | the first result |
| `messageLine` | the first line of the error message |
| `launch`, `branch`, `environment` | `#12 nightly`, the launch's branch and environment |
| `launchUrl`, `resultUrl` | links to FastProof (need `FASTPROOF_PUBLIC_URL`) |
| `fingerprint`, `count`, `defect`, `project`, `user` | failure fingerprint, number of results, defect type, project key, who creates the issue |
| `message`, `trace`, `tests` | blocks, each alone on its line: the message and the first 60 lines of the trace in code blocks, the list of up to 20 tests |

A line whose placeholders are all empty is left out. Values from test results are untrusted: they are escaped
for the tracker's markup so they cannot add links, mentions, references, images or markup (in GitLab and GitHub
titles too), and messages and traces go into code blocks that the content cannot close. For GitLab no line of
the description can start with `/`, so test output cannot run quick actions such as `/close` or `/move`. The
only links are FastProof's own pages, and links the tracker answers with are kept only on its own host. The
preview in the UI shows the text and the markup as sent.

## State sync

Every followed issue has a state as the tracker names it (`In Progress`, `opened`, `closed (completed)`,
`Done`) and a category, `open` or `closed`: Jira's status category `done`, GitLab and GitHub `closed`, the
Azure DevOps `closedStates`.

- `sync: poll` reads open issues every `FASTPROOF_TRACKER_POLL_SECONDS` (default 10 minutes) and closed ones
  daily for 30 days (they can be reopened), then no more. Due issues of one tracker are read together: one Jira
  search (`key in (...)`), one GitLab `iids[]` request, one GitHub GraphQL query (an `issueOrPullRequest(number:)` alias for each, checked against the repository and number) or
  one Azure DevOps `workitemsbatch` call, up to 25 issues; a single due issue is read on its own. A single GitHub and
  GitLab read is conditional (`If-None-Match`); a batch read is not. Jira has no conditional reads: FastProof keeps the
  `updated` time of an issue and treats an unchanged one like a `304`. An issue the batch answer lacks (deleted, no
  access, another project) is reported like a `404`. When Jira refuses the whole search with a `400` (a key it
  cannot see), the group is read issue by issue. Issues of a tracker removed from the settings are not read any more.
- `sync: webhook` reads an issue when the tracker's webhook names it, and daily. Point the webhook at
  `https://fastproof.example.com/api/v1/hooks/trackers/{project}/{trackerId}` with the secret of
  `webhookSecretRef` (purpose `tracker-webhook`): as the GitHub webhook secret (`X-Hub-Signature-256`), the Jira webhook secret
  (`X-Hub-Signature`), the GitLab secret token (`X-Gitlab-Token`), or the Azure DevOps service hook basic
  authentication password. The webhook only schedules a read through the API, at most one per issue every
  5 seconds, never ahead of the backoff of an issue whose reads fail; its payload is never taken as the state.
  Unverified calls get the same `401` after the same minimum time; a project and tracker take at most 120
  calls a minute.
- `sync: off` never reads.

A failed read is tried again after a minute, doubled up to six hours. A `429`, or a `503` with
`Retry-After`, or GitHub's exhausted rate limit pauses every call to that tracker from the replica for as
long as the tracker asked (at most an hour); three network failures or `5xx` answers in a row pause it for two
minutes, doubled for each further failure. A create during a pause answers `429` with `Retry-After`. One
replica reads a given issue at a time (reads hold a lease), a round takes at most 25 issues of one project's
tracker (100 in all) and reads eight trackers at a time, so one slow tracker cannot hold up the others.

The state shows next to the issue link of a defect (result page, triage) and on the **Issues** page of the
project (`GET /api/v1/projects/{key}/tracker-issues`, `launches:read`; filters `state`, `unmuteSuggested`,
`tracker`, `resultId`, `launchId`). `POST /tracker-issues/lookup {"urls": [...]}` answers the followed issues
behind up to 100 issue URLs. **Read the state now** (`POST /tracker-issues/{id}/sync`, `results:triage`) reads at
once, but not within 5 seconds of the last read, not during another read and not while failed reads back off
(`429` with `Retry-After`); **Stop following** (`DELETE /tracker-issues/{id}`, `issues:write`) keeps the issue in
the tracker and on the defects.

## Closed issues and mutes

When a read finds an issue closed and tests linked to it (its results, or the tests of its failure cluster)
are still muted, the issue gets `unmuteSuggested`. The **Issues** page shows it with **Unmute linked tests**
(`POST /tracker-issues/{id}/unmute`, needs `results:triage` and `results:mute`; audited as
`tracker.issue.unmute`) and **Keep muted** (`POST /tracker-issues/{id}/dismiss`). Nothing is unmuted without a
person deciding.
