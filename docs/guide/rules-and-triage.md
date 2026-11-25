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

Triage needs `results:triage` (the `editor`, `agent` and `admin` roles); `mute` and `unmute` also need
`results:mute` (`editor`, `admin`), otherwise `403` with `details.required`. Triage by an AI agent token is
recorded with `source: ai` whatever the request says. It answers `{"updated": n}`. A mute applies to
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
| `log`, `signals` | Log lines and signals of failed attempts: see [Log patterns](#log-patterns) |

At least one condition besides `statuses` is required; every given condition must hold.

| Action | Meaning |
|---|---|
| `defectTypeId` | Set the defect type |
| `comment` | Set the comment |
| `issue` | `{url, key?}` |
| `mute` | `{days?}`: mute the test for 1 to 365 days, or without an end |
| `accept` | Mark the failure as accepted: a known, tolerated failure |
| `pattern` | Mark the result with a log pattern: see [Log patterns](#log-patterns) |

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

## Log patterns

A rule can also look at the **logs** of failed attempts, at **signals** about them, and can **mark** results
with a named log pattern (like ReportPortal's pattern analysis). These rules belong to
[FastProof Analysis](analysis.md): they run in the analysis role (`FASTPROOF_ANALYSIS=on`) once a launch has
finished and settled, never on ingest, because log lines often arrive after the result (agents that batch
them, live log streams). The **Rules** page shows when each rule runs: *on ingest* or *after launch*.

```json
{
  "name": "Out of memory",
  "match": {
    "log": {"contains": "OutOfMemoryError", "levels": ["error"], "lastBytes": 262144}
  },
  "action": {"defectTypeId": "0193a5b2-...", "pattern": "oom"}
}
```

| Condition | Meaning |
|---|---|
| `log.regex` or `log.contains` | Exactly one: a regular expression on each log line (Rust syntax), or a case-insensitive text |
| `log.levels` | Only lines of these levels (`trace`, `debug`, `info`, `warn`, `error`) |
| `log.streams` | Only lines of these streams (`stdout`, `stderr`, `log`, `step`) |
| `log.lastBytes` | Read only the last bytes of each attempt's log: 1024 to 1048576 (default 256 KiB, `FASTPROOF_ANALYSIS_LOG_LAST_BYTES`) |
| `signals` | 1 to 5 `{source, kind?}`: the result has a stored [signal](signals.md) of that source (`mock`, `grid`, `ci`, `k8s`) and kind |

| Action | Meaning |
|---|---|
| `pattern` | Mark the result with this pattern (`^[a-z0-9][a-z0-9._-]{0,63}$`); only a mark, the defect stays. Combinable with the other actions |

`log` and `signals` add to the other conditions (all must hold). A rule needs at least one condition and one
action; `log` or `signals` alone is enough, and so is `pattern` alone as the action.

What the rule reads, per launch:

- the **failed and broken attempts**, newest first, at most `FASTPROOF_ANALYSIS_LOG_ATTEMPTS` (2000);
- of each, the **last `lastBytes`** of its live log (the lines reporters stream to
  `POST .../launches/{id}/logs`, decompressed one chunk at a time), or, when the attempt has no live log, of the
  `stdout` and `stderr` captured in its uploaded result. Captured output has no levels: a rule that names
  `levels` skips it. Each line counts its bytes and one for its end; a tail holds at most 131072 lines;
- every line cut to 16 KiB; `contains` is a case-insensitive literal search; patterns compiled with the same limits as the other rules (linear time, no
  backreferences or lookaround, bounded size); the time spent decoding and matching is at most
  `FASTPROOF_ANALYSIS_LOG_CPU_MS` (2 s) per launch, checked line by line.

When a budget runs out, the launch's `logRules` step (`GET .../launches/{id}/analysis`) is `partial` and its
`detail.reason` says which: `attempts`, `log_cpu` or `regex`; the server counts it in
`fastproof_analysis_budget_exhausted_total{kind}`.

Effects:

- **Pattern marks** appear on the result (tab **Triage**, with the matching line, at most 300 characters,
  control characters, colour codes and invisible bidirectional and zero-width characters removed; lines are shown as the reporter sent them, so mask secrets at
  the source), in `GET .../results/{id}` as `patterns`, as a filter `GET .../launches/{id}/results?pattern=oom`,
  in `GET .../analysis/patterns?days=30&limit=20` (results and launches per pattern) and in the dashboard widget
  **Most popular error patterns** with its setting *Count: log patterns marked by rules*
  (`settings.patterns: "logPatterns"`). A new analysis of a launch replaces the marks these rules made there.
- **Other actions** (defect, comment, issue, mute, accept) are applied like a retroactive application: the
  first matching rule in `order` per result, results a person or an agent triaged are left alone, and the
  changes of each rule are one batch in the triage history (`via: retro`, by `rule:<id>`).

**Undo.** A batch of a rule that ran after the launch, or of an application to past launches, is undoable:
**Undo** in the result's triage history or `POST .../triage/undo {"batchId": "..."}`. Only the results that
still have the rule's decision are restored; mutes the batch created are removed, and a series that was muted
before keeps its mute as it was (a rule never changes an existing mute when it runs after the launch). An
undone batch comes back when the launch is analysed again, so disable or change the rule too. (Batches of rules applied on ingest are not undoable: the next
ingest would apply them again; change the rule instead.)

**Dry run** of such a rule (`POST .../rules/test`) reads the logs of the chosen launch the same way and writes
nothing. **Apply to past launches** queues the task `log_rule.apply` for the analysis role: it reads the
launches within the project's raw retention (`retention.rawDays`), each within the per-launch budget and the
whole task within 5 minutes; `GET .../tasks/{id}` shows `matched`, `marked`, `updated` and whether it was
`partial`. Applying needs FastProof Analysis on (409 `analysis_disabled` otherwise); at most two such
applications wait or run per project (429 for a third).

Limits: at most `FASTPROOF_ANALYSIS_MAX_LOG_RULES` (100) enabled after-launch rules per project (409
`limit_reached`, `details {limit: "maxLogRules", max}`). During an upgrade from a version without these rules,
writing one (create, change, agent draft, apply, and a dashboard with `settings.patterns`) answers 409
`upgrade_in_progress` until every replica runs this version: an older replica would read such a rule as one
that matches every failure.

## Flaky tests and mutes

A test is flaky when its status flips within the last runs of its series (settings on the **Settings** page:
history depth, stabilization period, whether passed retries count). `GET /api/v1/projects/{key}/flaky` and
the **Flaky** page list them. Muted tests stay visible in launches, but their failures are not counted
by [quality gates](quality-gates.md).
