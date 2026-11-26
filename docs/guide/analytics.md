# Analytics

The project's **Analytics** page answers questions about many launches at once: which errors fail most,
how defect groups change over time, how long errors take to fix, how much time retries cost, which steps
are slow, which tests became slower, and, across projects, how healthy each one is. It is part of
[FastProof Analysis](analysis.md) and appears in the project menu when the analysis role is on
(`FASTPROOF_ANALYSIS=on`, see [Configuration](../operations/configuration.md)).

The numbers are plain counts and percentiles over stored results. Nothing here is a prediction or a
self-learning model.

## Where the numbers come from

| | Without ClickHouse | With ClickHouse |
|---|---|---|
| Window | The last 30 days at most | Up to 365 days (as far as the ClickHouse TTL keeps data) |
| Project views | From PostgreSQL | From ClickHouse |
| Portfolio and similar failures in other projects | Not available (409 `clickhouse_required`) | Available |

Without ClickHouse the page shows the banner "Last 30 days; longer history and the portfolio need
ClickHouse", and a longer window is answered for its last 30 days (`window.partial: true` in the API, with a
`reason`). Each card says which backend answered. If ClickHouse is configured but does not answer, the
project views fall back to PostgreSQL instead of failing.

ClickHouse is used when the ClickHouse sink is on (`FASTPROOF_SINKS` contains `clickhouse`,
`FASTPROOF_CLICKHOUSE_URL` set) and `FASTPROOF_ANALYSIS_CLICKHOUSE=auto` (the default). See
[Scaling](../operations/scaling.md#clickhouse-or-opensearch) for when it is worth running.

## The views

Pick the window (7, 30, 90 days or a year) and, optionally, a branch. Days are UTC days.

- **Top failures**: failed and broken results grouped by **unique error** (fingerprints merged on the
  [Unique errors](unique-errors.md) page count in their error), **defect group** (the current triage; results
  nobody triaged count as "To investigate"), **suite** or **environment**. Each group shows its failures, the
  tests and launches it touched, its share of all failures and a trend of up to 14 buckets of whole days.
- **Defect groups over time**: the same failures by defect group per bucket, so you see whether product bugs
  or system issues dominate this week.
- **Flaky results**: per day, the share of results that passed only after a retry.
- **Time to resolve errors**: unique errors resolved in the window, from first seen to resolved, with the
  median, the 90th percentile and how many were fixed within an hour, six hours, a day, three days, a week,
  30 and 90 days. It reads the error catalog, so it is the same on both backends.
- **Retry cost**: the time spent on attempts that did not count (every attempt of a result except its last),
  in total and per suite, with the number of retries and of tests that retried.
- **Slow steps**: step names with the largest 95th percentile duration, with the median and the number of
  runs. Without ClickHouse it reads the steps of the newest 2,000 attempts of the window.
- **Slower tests**: tests whose median duration in the last 7 days is at least 1.3 times the median of the 28
  days before, with at least 10 runs in each part (passed, failed or broken runs). Without ClickHouse the
  comparison uses the 23 days before.

The error timeline on a [unique error](unique-errors.md) uses the same backends: with ClickHouse it covers up
to a year from a daily aggregate.

## Portfolio

The **Portfolio** tab (and `GET /api/v1/analytics/portfolio`) lists, for every project you can read (at most
200, `projects=a,b` narrows the list): launches, pass rate of the results that ran, failures, unique errors
first seen and regressed in the window, flaky tests and the median time to resolve. A project you cannot read
(`launches:read`) never appears and never counts in any number. It needs ClickHouse.

`GET /api/v1/analytics/similar?project=<key>&resultId=<id>` finds failures like a result in the **other**
projects you can read: the same fingerprint scores 1, a close normalized message less. It also needs
ClickHouse.

## API

All project views need `launches:read` and take `from` and `to` (RFC 3339, default the last 30 days), and
most take `branch`, `env` and `limit` (1 to 100):

```bash
curl -H "Authorization: Bearer $TOKEN" \
  "https://fastproof.example.com/api/v1/projects/web-shop/analytics/failures?groupBy=defectGroup&from=2026-10-01T00:00:00Z"
```

| Endpoint | Answers |
|---|---|
| `GET …/analytics/failures` | `groupBy` = `fingerprint` (default), `defectGroup`, `suite`, `env` |
| `GET …/analytics/flaky-trend` | flaky and run results per day |
| `GET …/analytics/mttr` | time to resolve errors |
| `GET …/analytics/retries` | retry cost per suite |
| `GET …/analytics/slow-steps` | slowest step names |
| `GET …/analytics/durations` | tests that became slower |
| `GET /api/v1/analytics/portfolio` | the portfolio (ClickHouse) |
| `GET /api/v1/analytics/similar` | similar failures in other projects (ClickHouse) |

Every answer carries `window {backend, from, to, partial, reason?}`. These reads are heavy queries and share
the per-caller limit of other heavy reads (429 with `Retry-After` when exceeded). There is no MCP tool for
these views yet; agents read them through the REST routes above.

## What ClickHouse keeps for Analysis

From schema version 2 (created on start; the `schema_version` table records it) ClickHouse keeps, next to
the raw results, attempts, steps and logs:

- the defect of each result (type, group, who set it), its mute and the error it is merged into; a result is
  written again after every triage change;
- the fingerprint, top stack frames and trace id of each attempt;
- `triage`: people's, agents' and rules' decisions (no rule decisions made at ingest), used for longer
  suggestion history than the PostgreSQL memory keeps;
- `fingerprint_daily`: occurrences, tests and launches of each fingerprint per day, branch and environment;
- `defect_daily`: failures per defect group per day, recomputed every hour for the last 14 days, project
  by project (a project whose rollup fails is logged and the others go on).

The history and the daily tables are kept for `FASTPROOF_CLICKHOUSE_AGG_TTL_DAYS` (default 730, Helm
`sinks.clickhouse.aggTtlDays`); raw rows for `FASTPROOF_CLICKHOUSE_RAW_TTL_DAYS`. After an upgrade, the
analysis role sends the decisions journaled before and writes the defect columns of results within the raw
TTL again (job `analysis.ch.backfill`, a few thousand rows a minute), then recomputes `defect_daily` once over
the raw TTL. Deleting a launch or a project removes its rows from every table; the daily aggregates of the
deleted launch's days are rebuilt from the remaining results (days older than the raw TTL keep their
aggregates). If that rebuild fails, the deletion still completes, the failure is counted in
`fastproof_clickhouse_rebuild_errors_total`, and the hourly rollup corrects `defect_daily` for the last 14 days.

Every read runs read-only with a 30-second limit and is cancelled when the request that started it goes away.
