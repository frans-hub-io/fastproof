# Rules and triage

Triage says why a test failed: a product bug, a broken test, an environment problem. FastProof groups
failures so one decision covers many results, carries decisions forward with rules, and keeps every change in
the audit log.

## Defect types

Every failed or broken result gets a defect. Five groups are built in, each with a built-in type:

| Group | Built-in type | Meaning |
|---|---|---|
| `productBug` | Product bug (PB) | The product behaves wrongly |
| `automationBug` | Automation bug (AB) | The test, its data or its locators are wrong |
| `systemIssue` | System issue (SI) | The environment, a dependency or the infrastructure failed |
| `noDefect` | No defect (ND) | Expected, or not a problem |
| `toInvestigate` | To investigate (TI) | Not looked at yet: the default of every new failure |

Projects add their own types inside a group (up to 100, with a name, a short name and a color) on the
**Settings** page or with `POST /api/v1/projects/{key}/defect-types`. Dashboards and reports count by group,
so custom types stay comparable across projects.

A result's defect is
`{typeId, group, name, comment, issueUrl, issueKey, source, ruleId, at, by, accepted}`. `source` says who set
it: `user`, `ai` (an agent through [MCP](mcp-copilot.md)), `rule`, or `fastproof` (the default).

## Clusters

Failures are grouped by a **fingerprint**: the error message with numbers, hex ids, UUIDs, timestamps,
temporary paths and line numbers removed, plus the top stack frames outside test frameworks. ANSI colour
codes are stripped from messages and traces at ingest, so the same failure reported by Playwright JSON (which
colours its messages) and by JUnit or a reporter gets the same fingerprint. The launch page shows the clusters, largest first, and a decision on
a cluster applies to every result in it:

```http
GET /api/v1/projects/{key}/launches/{id}/clusters
GET /api/v1/projects/{key}/results/{id}/similar
```

`similar` finds results in other launches that failed the same way (same fingerprint first, then similar
text) and shows how they were triaged.

## Triage

```http
POST /api/v1/projects/{key}/triage
```

```json
{
  "launchId": "0193a5b2-...",
  "fingerprint": "3f9c2a7d1b0e4c55",
  "defectTypeId": "0193a5b2-...",
  "comment": "Payment sandbox returns 503 since the 10:00 deploy",
  "issue": {"key": "PAY-1234"},
  "mute": {"until": "2026-12-01T00:00:00Z", "reason": "waiting for PAY-1234"}
}
```

| Field | Meaning |
|---|---|
| `resultIds` | 1 to 1000 results, or |
| `launchId` + `fingerprint` | every result of the launch in that cluster |
| `defectTypeId` | The new defect type |
| `comment` | At most 4000 bytes; an empty string removes it |
| `issue` | `{url}` or `{key}`; a key alone becomes a URL through the project's [issue trackers](notifications.md#issue-trackers) |
| `mute` | `{until?, reason}`: hide the test from gates and failure counts until a date (at most 10 years), or until unmuted |
| `unmute` | `true` lifts the mute |

Triage needs `results:triage` (the `editor` and `admin` roles) and answers `{"updated": n}`. A mute applies to
the test, so its later results stay muted until it ends. Every change is written to the audit log.

## Rules

Rules triage known failures automatically, like Allure resolutions. Manage them on the project's **Rules**
page or through the API (needs `rules:write`).

```json
{
  "name": "Payment sandbox outage",
  "enabled": true,
  "order": 10,
  "match": {
    "messageRegex": "payment-sandbox.*(503|Service Unavailable)",
    "environment": "staging"
  },
  "action": {
    "defectTypeId": "0193a5b2-...",
    "comment": "Known sandbox outage",
    "issue": {"url": "https://jira.example.com/browse/PAY-1234", "key": "PAY-1234"},
    "mute": {"days": 3}
  }
}
```

| Condition (`match`) | Meaning |
|---|---|
| `messageRegex`, `traceRegex` | Regular expression on the error message or stack trace (first 16 KiB) |
| `testCaseId` | One test |
| `fingerprint` | One cluster (16 hex digits) |
| `environment` | Environment key of the launch |
| `statuses` | Result statuses; `failed` and `broken` by default |

At least one of the first five is required; every given condition must hold.

| Action | Meaning |
|---|---|
| `defectTypeId` | Set the defect type |
| `comment` | Set the comment |
| `issue` | `{url, key?}` |
| `mute` | `{days?}`: mute the test for 1 to 365 days, or without an end |
| `accept` | Mark the failure as accepted: a known, tolerated failure |

How rules run:

- **On ingest.** Each new failed or broken result is checked against the enabled rules in `order` (then
  creation time); the first match applies, and its hit counter grows. Without a match the result gets the
  default defect, To investigate.
- **People win.** A defect set by a person or an agent (`source` `user` or `ai`) is never replaced by a rule
  or the default.
- **Dry run.** `POST .../rules/test` with `{"rule": {...}, "launchId": "..."}` returns the results the rule
  would match in that launch, without changing anything. The **Rules** page runs it before you save.
- **Apply to past launches.** `POST .../rules/{id}/apply?launchId=...` applies one rule to one launch, or
  without `launchId` to the launches of the last 30 days (at most 500). It runs as a background task:
  `GET .../tasks/{id}` reports `queued`, `running`, `done` or `failed` and the progress.

Patterns use the Rust `regex` syntax, which matches in linear time: no backreferences or lookaround.
Patterns have at most 1 KiB and a bounded compiled size, a project has at most 100 rules, and each evaluation
pass has a work budget; when it runs out, the remaining results get the default defect and the server counts
it in `fastproof_rule_budget_exhausted_total`.

## Flaky tests and mutes

A test is flaky when its status flips within the last runs of its series (settings on the **Settings** page:
history depth, stabilization period, whether passed retries count). `GET /api/v1/projects/{key}/flaky` and
the **Flaky** page list them. Muted tests stay visible in launches, but their failures are not counted
by [quality gates](quality-gates.md).
