# Quality gates

A quality gate passes or fails a finished launch on its numbers, so a pipeline can stop on a bad run and a
merge request shows the verdict. Gates are project settings: edit them on the project's **Gates** page or in
the `gates` key of `PUT /api/v1/projects/{key}/settings` (needs `settings:write`).

## A gate

```json
{
  "gates": [
    {
      "name": "main is green",
      "branches": ["main", "release/*"],
      "rules": [
        {"metric": "failed", "op": "lte", "value": 0},
        {"metric": "passRate", "op": "gte", "value": 98},
        {"metric": "newFailures", "op": "lte", "value": 0, "baseline": "main"},
        {"metric": "duration", "op": "lte", "value": 1800}
      ]
    }
  ]
}
```

| Field | Meaning |
|---|---|
| `name` | 1 to 100 bytes, unique in the project |
| `enabled` | `true` by default; a disabled gate is kept but not evaluated |
| `branches` | Branch globs (`*` matches any text); every branch when absent |
| `rules` | 1 to 20 rules; every rule must hold |

A rule holds when `actual op value`, with `op` one of `gt`, `gte`, `lt`, `lte`:

| Metric | Actual value |
|---|---|
| `failed` | Failed results, muted ones excluded |
| `broken` | Broken results, muted ones excluded |
| `newFailures` | Failed or broken results whose test was not failing in the comparison launch |
| `passRate` | Percent of passed results among executed ones (total minus skipped), 0 to 100 |
| `flaky` | Results that were flaky within the run (passed after a retry) |
| `duration` | Launch duration in seconds; for a launch with a [manual run](manual-runs.md#combined-launches-and-closing-a-run), the duration of its automated part |
| `newErrors` | Unique errors of the launch that are new, see [New errors, to investigate and total](#new-errors-to-investigate-and-total) |
| `toInvestigate` | Failed or broken results, not muted, that have no defect or one of the group To investigate |
| `total` | Results that ran: every status except skipped |

Without `baseline`, `newFailures` compares with the previous launch of the same branch. With
`baseline` (a branch name), `newFailures` compares with the newest finished launch of that branch, and the
other metrics become the difference to that launch: `{"metric": "failed", "op": "lte", "value": 0,
"baseline": "main"}` means "no more failures than on `main`". A project has at most 20 gates and at most 5
distinct baselines. When the comparison branch has no finished launch yet, `newFailures` counts every
failure as new, and the other rules with a `baseline` hold; both carry a `note` in the result.

## New errors, to investigate and total

Three more metrics come with FastProof Analysis. They are ordinary rules, so they work with `baseline` and
in every place a gate is used (the Gates page, `fastproof gate`, notifications and merge request comments).

- `newErrors` counts the launch's [unique errors](unique-errors.md) that were not seen before. Without
  `baseline` an error is new when it has not occurred on the launch's own branch in the last 30 days; with
  `baseline: "main"` it is new when it has not occurred on `main` in that time, which is the rule to use on a
  feature branch ("this branch brings no error that `main` does not have"). Errors you set to ignored are
  not counted. With a `baseline` this metric is not a difference: it is the count itself, as for `newFailures`.
- `toInvestigate` counts the failures nobody has classified yet: failed or broken results that are not muted
  and have no defect, or have one of the group To investigate. A rule `toInvestigate lte 0` therefore means
  "every failure of this launch was triaged". It is the counterpart of the To investigate counter that
  ReportPortal shows on each launch.
- `total` counts the results that ran. A rule `total gte 500` catches a run that silently executed too few
  tests, similar to the minimum test count of other gate implementations. With a `baseline`, it is the
  difference to the baseline launch, so `total gte -20` allows at most 20 tests fewer than on `main`.

```json
{"name": "triaged and nothing new", "rules": [
  {"metric": "toInvestigate", "op": "lte", "value": 0},
  {"metric": "newErrors", "op": "lte", "value": 0, "baseline": "main"},
  {"metric": "total", "op": "gte", "value": 500}
]}
```

!!! note "Mixed versions"
    A gate that uses `newErrors`, `toInvestigate` or `total` is saved only when every running replica is on
    a version that knows them; until then `PUT .../settings` answers `409 upgrade_in_progress` with
    `details.area: "analysis"` (see [Upgrading](../operations/upgrading.md)). A replica of an older version
    could not read such a gate, so this protects the evaluation, not the data. Gates saved before the
    upgrade are not affected.

## When gates are evaluated

- The gate result of a launch is `pending` while the launch runs or has uploads that are not processed yet.
- When a launch is finished and its uploads are processed (or after 30 minutes of waiting for them), the
  server evaluates every enabled gate whose `branches` match the launch branch. The result is `failed` when
  any rule does not hold, otherwise `passed`. A branch without gates passes with no rule results.
- The result is stored with the launch, sent to [notifications](notifications.md) and posted to the merge
  request of the project's pinned repository (see
  [Merge request comments](notifications.md#merge-request-comments-and-commit-statuses)), and recomputed on
  demand by the API.

```http
GET /api/v1/projects/{key}/launches/{id}/gate
```

```json
{
  "status": "failed",
  "launchId": "0193a5b2-...",
  "evaluatedAt": "2026-11-21T10:12:00Z",
  "results": [
    {"gate": "main is green", "rule": {"metric": "failed", "op": "lte", "value": 0},
     "actual": 3, "expected": 0, "ok": false}
  ]
}
```

`actual` is `null` when a metric cannot be measured (for example `passRate` without executed results); such a
rule does not hold.

## History

With Analysis on, the Gates page has a **History** tab and
`GET /api/v1/projects/{key}/gates/history` answers the same data (`launches:read`). Every stored verdict is
listed newest first, with the failed rules and the measured value, and the first page carries aggregates for the window:

- per gate: evaluations, failures, **flips** (how often the verdict changed between passed and failed on a
  branch), the mean time **to green** (from the first failed launch of a streak to the next passing one on the
  same branch; a streak that is still red is not counted), and the metric that failed most;
- per branch: evaluations and failures.

Filters: `gate`, `branch`, `from` and `to` (RFC 3339; the default window is the last 30 days, at most 365) and
`before` and `limit` for paging. With `gate`, the verdict and failed rules of that gate alone are shown.
The aggregates cover the newest 20 000 evaluations of the window; launches without a branch are in the gate
totals but not in the branch list. A launch that was still pending, or that ran no gate at all, is not listed.

```http
GET /api/v1/projects/example/gates/history?gate=main%20is%20green&branch=main&limit=2
```

```json
{
  "items": [
    {"launchId": "0193a5b2-...", "launchNumber": 412, "branch": "main", "evaluatedAt": "2026-11-24T09:15:00Z",
     "status": "failed",
     "failed": [{"gate": "main is green", "metric": "newErrors", "actual": 2, "expected": 0}]}
  ],
  "next": "1795598100000000_0193a5b2...",
  "gates": [{"gate": "main is green", "evaluations": 87, "failed": 9, "flips": 7,
             "meanSecondsToGreen": 5400, "topFailingMetric": "newErrors"}],
  "branches": [{"branch": "main", "evaluations": 87, "failed": 9}]
}
```

## Simulation

Before you tighten a gate, see what it would have done. The **Simulation** tab (or
`POST /api/v1/projects/{key}/gates/simulate`, `launches:read`) evaluates a draft gate on the newest 1 to 200
finished launches (default 50, optionally of one `branch`) from the stored results and writes nothing: no
verdict is stored, nobody is notified. The draft is validated like the settings and is evaluated whether or
not it is enabled.

```http
POST /api/v1/projects/example/gates/simulate
```

```json
{"gate": {"name": "draft", "rules": [{"metric": "toInvestigate", "op": "lte", "value": 0}]},
 "launches": 100, "branch": "main"}
```

The answer has `evaluated`, `failed`, `skipped` and, per launch, the `status` and every rule's result as
`GET .../gate` shows it. A launch is skipped when the draft's `branches` do not match it, when it still has
unprocessed uploads, or when its results are no longer stored (retention). For a launch the result is the
one the real evaluation gives at the time you ask, so a metric that depends on history (`newErrors`) is
computed from today's history of errors, not the history at the time the launch finished. A simulation counts
against the per-minute limit for expensive writes and takes a heavy-query slot (429 when either is used up).

## In CI

`fastproof gate` checks the gate of the current launch and sets the exit code: `0` passed, `1` failed, `2`
pending or an error. Run it after `fastproof launch finish` and let it wait for processing:

```sh
fastproof launch finish
fastproof gate --wait 10m
```

The launch is found by `--launch` (or `FASTPROOF_LAUNCH`, else the CI pipeline) or `--id`. `--poll` sets the
interval while waiting (default `5s`). The token needs `launches:read`.

## Quality metrics

The operations listener (`/metrics`) exports per-project quality metrics next to the server metrics:

| Metric | Meaning |
|---|---|
| `fastproof_project_pass_rate{project,branch,environment}` | Passed / executed of the newest finished launch of each branch and environment in the last 30 days (0 to 1) |
| `fastproof_project_flaky_tests{project}` | Tests flaky within a run in the last 20 launches |
| `fastproof_launch_duration_seconds{project}` | Histogram of finished launch durations |

To keep the number of series bounded, only the most active branches of a project get their own `branch`
label (`FASTPROOF_METRICS_TOP_BRANCHES`, default 10) and the five most active environments their own
`environment` label; the others are reported as `other`.
