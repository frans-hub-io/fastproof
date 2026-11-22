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

Without `baseline`, `newFailures` compares with the previous launch of the same branch. With
`baseline` (a branch name), `newFailures` compares with the newest finished launch of that branch, and the
other metrics become the difference to that launch: `{"metric": "failed", "op": "lte", "value": 0,
"baseline": "main"}` means "no more failures than on `main`". A project has at most 20 gates and at most 5
distinct baselines. When the comparison branch has no finished launch yet, `newFailures` counts every
failure as new, and the other rules with a `baseline` hold; both carry a `note` in the result.

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
