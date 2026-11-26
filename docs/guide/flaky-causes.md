# Flaky causes

The **Flaky tests** page says *which* tests change their outcome without a code change (see
[Concepts](concepts.md#retries-and-flaky-tests)). With [FastProof Analysis](analysis.md) on, it also says *why
they probably do*: each flaky test gets up to six likely causes, each with the evidence it rests on and the
actions it suggests.

This is a transparent, rule-based search over the test's recent runs, not a self-learning model: every cause
has a fixed rule and a minimum amount of data, and the panel shows the numbers behind it. A cause never mutes a
test, files an issue or changes a defect by itself; a person decides.

Flaky causes need the analysis role (`FASTPROOF_ANALYSIS=on`, see
[Configuration](../operations/configuration.md)) and a project that has not turned Analysis off
(`analysis.enabled`).

## Which tests get causes

A test here is a **series**: one test in one environment (the same test in `staging` and in `production` are
two series). A series gets causes when it is marked flaky, or its flakiness score is at least
`analysis.flaky.hintMinScore` (default 0.2, from 0.05 to 1.0; **Settings → Analysis → Flaky causes**):

```json
{"analysis": {"flaky": {"hintMinScore": 0.3}}}
```

Causes are computed after every launch in which the series changed status (a new failure, a fix, flaky against
earlier runs or within the run; up to 200 series and 30 seconds per launch), and once a day for every other flaky
series whose causes are older than a day. Causes of series that are no longer flaky are dropped by the daily job.
The daily job runs for at most 30 minutes, shared between the projects (each gets the time left divided by the
projects left, and the first project changes every day), so one large project cannot hold back the others; series
left over come first the next day. When ClickHouse does not answer, the rest of the run reads PostgreSQL.

## The window

| Storage | Runs read |
|---|---|
| PostgreSQL | the newest 50 runs within the project's raw retention (`retention.rawDays`); debug launches are left out |
| ClickHouse | the whole history ClickHouse keeps; adds the `time` cause |

Skipped and unknown runs are not counted. A **failure** is a run that failed or broke, or one whose first
attempt failed and a retry passed.

## Causes

| Cause | Rule | Minimum |
|---|---|---|
| **Timing** | Most failures are timeouts (the message says "timeout", "timed out", "deadline exceeded"…), or failures stop at a steady duration (± 5 %) above where passing runs usually end, with the p95 of passing runs at 70 % of it or more | 5 failures |
| **Environment** | One host, CI runner (launch attribute `runner`) or environment fails at least 3 times as often as the others, and Fisher's exact test (two-sided) gives p < 0.05. Environments are compared across the series of the same test with the same parameters | 5 runs with the value and 5 without |
| **Test order** | Fails together with another test (Jaccard similarity of their sets of failing launches ≥ 0.6, at least 3 shared), or fails only after a certain test ran earlier on the same worker (labels `host` and `thread`; that test precedes ≥ 80 % of the failures and ≤ 20 % of the clean passes, Fisher p < 0.05) | 10 runs |
| **Passes on retry** | Fails, then passes on a retry of the same run, with different errors each time (one stable error that goes away on retry is a known failure, not chance) | 3 such runs |
| **Infrastructure** | The failure has a [signal](signals.md) from a grid, CI, Kubernetes or a mock, an infrastructure error (connection refused, DNS, 502/503/504, out of memory, browser crashed, session lost), or falls into a burst: within 60 seconds of it, at least 20 % of the launch's results failed with infrastructure errors | 2 matches |
| **Time of day** | Fails at least 3 times as often in one six-hour block of the day or on one weekday (UTC), Fisher p < 0.05 | ClickHouse only, 30 runs |

Each cause carries a confidence (`high`, `medium`, `low`) from the strength of its evidence (for example the
p-value, the Jaccard similarity or the number of matches). The list shows the two most confident causes as
chips in the **Likely cause** column; a click opens the panel with every cause, its evidence table and the
newest runs that support it.

### Infrastructure share

Failures with infrastructure evidence stay in the flakiness score: excluding them would hide real instability
behind a noisy environment. The list and the panel show their share separately ("infra 40 %").

## Actions

Each cause suggests what can be done; nothing happens until a person clicks.

| Action | What it does | Needs |
|---|---|---|
| **Mute with an end** | Mutes the series for 1, 7, 14 or 30 days through the usual triage mute, with the cause as the reason. The mute is in the triage journal and can be undone | `results:triage` and `results:mute` |
| **Issue** | Opens the issue dialog for the newest failing result of the series | `issues:write` |
| **Rule draft** | Creates a **disabled** rule for the test (timeouts as an automation bug; infrastructure trouble as a system issue, matching the top signal or infrastructure messages) for a person to review and enable on the **Rules** page | `rules:write` |

## API

```bash
curl -s -H "Authorization: Bearer $FASTPROOF_TOKEN" \
  "https://fastproof.example.com/api/v1/projects/shop/flaky/0193f0c2-5b7e-7c41-9a50-2f8d6c1e4b11/hints"
```

```json
{
  "seriesId": "0193f0c2-5b7e-7c41-9a50-2f8d6c1e4b11",
  "computedAt": "2026-11-25T02:41:07Z",
  "windowRuns": 30,
  "failures": 6,
  "infraFailures": 0,
  "infraShare": 0.0,
  "backend": "postgres",
  "hints": [
    {
      "kind": "environment",
      "confidence": "medium",
      "summary": "Fails on host ci-3: 5 of 10 runs there against 1 of 20 elsewhere (p = 0.0116)",
      "evidence": {"dimension": "host", "value": "ci-3", "runs": 10, "failures": 5, "otherRuns": 20,
                   "otherFailures": 1, "ratio": 10.0, "p": 0.0116, "examples": [{"launchId": "…", "resultId": "…"}]},
      "sample": 10,
      "actions": ["mute", "issue"]
    }
  ]
}
```

`GET /api/v1/projects/{key}/flaky/{seriesId}/hints` needs `launches:read`; `computedAt` is `null` before the
first computation. `GET /api/v1/projects/{key}/flaky` gives every flaky test its `seriesId`, the `hints` (kind
and confidence) and `infraShare`.

## Limits

- Order evidence reads the newest 20 runs of the window (the launches they ran in), at most 10 earlier tests
  per run on the same worker within 30 minutes, and at most 20 other tests failing in the same launches.
- Environments are compared across at most 20 environments of the same test, the newest 50 runs of each
  within raw retention (the test's own environment is always one of them).
- Host and thread come from result labels (`host`, `thread`, as Allure writes them); the CI runner from the
  launch attribute `runner` (or `ci.runner`).
- On PostgreSQL the time-of-day cause is never shown: 50 runs of a few days cannot tell a nightly pattern from
  chance.
