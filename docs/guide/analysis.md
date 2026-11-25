# Triage suggestions

FastProof Analysis remembers how people triaged failures and suggests the same decision when
the same error, or a very similar one, fails again. It also keeps the decision history of every
result, so any batch of triage can be undone.

Suggestions are a **transparent nearest-decision search**: every suggestion is the sum of a
fixed table of weights over what people decided before, and the page shows each row of that
sum. It is **not a self-learning model**: nothing is trained, nothing changes by itself, and the
same history always gives the same suggestion.

Suggestions need FastProof Analysis on the instance (`FASTPROOF_ANALYSIS=on`, see
[Configuration](../operations/configuration.md)) and the project (Settings → Analysis →
"Analyse this project"). The decision history and undo work without it.

## Where suggestions show

- **Launch → Clusters.** Each cluster (the failures of a launch with one fingerprint) shows a
  chip with its first suggestion, for example `Product bug · high · as in #431, PAY-1234`: the
  defect type, the band, the launch of an earlier decision and its issue. Open the cluster for
  up to three suggestions (one per defect type) with **Accept**, **Other** (triage with another
  type), **Reject**, **Make a rule** and **Why**.
- **Result page.** The "Suggestions" panel above "Similar failures" shows the suggestions of the
  result's cluster with the same actions.
- A cluster that a person or a rule already decided gets no suggestion. A cluster without one
  says why: a new error, no triage history in the project yet, or no earlier decision close
  enough.

Suggestions are computed after a launch finishes (and again with **Recompute**, at most every
30 seconds per launch).

## Bands and "why"

| Band | Meaning |
|---|---|
| `same` | The same error (fingerprint or one of its aliases) was decided this way before, with no other type remembered for it and never overridden: by the same test at least once, or three times in any test. |
| `high` | Score 0.70 or more. |
| `low` | Score from the project's minimum (0.40 by default) up to 0.70. |

The score is the sum of these rows, clamped to 0..1; "Why" lists each one that applied:

| Row | Adds |
|---|---|
| The same fingerprint | +0.60 |
| Message similarity *s* (another fingerprint; below 40% there is no candidate) | +0.45 × *s* |
| Shared stack frames (Jaccard *j*) | +0.20 × *j* |
| A test of the cluster was decided this way | +0.10 |
| The same environment / branch | +0.05 / +0.03 |
| Confirmations *n* (people chose it again) | + min(0.10, 0.03 × *n*) |
| Overrides *m* (people changed it to another type) | − 0.15 × *m* |
| Age of the last decision, *d* days | × (1 − min(0.3, *d* / 365)) |
| Another type remembered for the same fingerprint | both shown, each − 0.10 |
| A disabled rule whose dry run matches the cluster | 0.55 (fixed) |
| Signals of the cluster's attempts (grid, CI, Kubernetes: system issue; a mock's unmatched request: automation bug) | +0.30 exact, +0.15 by time window |

"Why" also shows the sample decision (who, when, in which launch, how often confirmed and
overridden), its comment and issue, and the commit range when the error has one.

The band's percentage (for example `high 91%`) appears only after a [backtest](#backtest) had
at least 50 decisions in that band in this project.

## Accept, reject, rules

- **Accept** applies the suggestion's defect type (with a comment or issue when you add one) to
  every result of the cluster that no person or agent decided; results decided by people stay
  as they are and are counted as skipped. It is one journal batch with `via: suggestion`, so it
  can be undone. An AI agent that accepts is recorded as `ai`.
- **Reject** closes the suggestion. It does not change what is remembered: only a person's
  different decision counts against an earlier one.
- **Make a rule** appears when people chose the same type for the same fingerprint at least
  three times without overriding it, and no rule matches that fingerprint yet. It creates a
  disabled rule `Same as <fingerprint>` for review under [Rules](rules-and-triage.md).

## What is remembered

Only people's decisions teach (`source: user`): decisions of rules, AI agents and auto-apply
never do. Confirm an AI or automatic decision (the "Confirm" button) to make it yours. The
memory keeps one entry per fingerprint and defect type with its confirmations, overrides, the
newest tests, environments and branches, the last comment and issue and a sample result. It
outlives raw results for `memoryDays` (365 by default). When a project turns Analysis on, the
triage people made earlier is learned once.

## Decision history and undo

The result page's **Triage** tab lists every change of the result's defect type or mute: when,
how (`Triage`, `AI agent (MCP)`, `ReportPortal`, `Rule at ingest`, `Rule applied later`,
`Accepted suggestion`, `Auto-apply`, `Undo`), who, and the state before and after.

**Undo** puts back the state before a batch for every result it changed that nobody changed
since; the others are listed as skipped (changed since, result gone, launch merged). The undo is
a batch itself, so it can be undone too. Rule batches at ingest cannot be undone (edit the
rule). Undoing a batch that changed mutes needs `results:mute`; an AI agent can undo only its
own batches. An undone decision is also taken back out of the memory and the backtest history.

```sh
curl -X POST https://fastproof.example.com/api/v1/projects/shop/triage/undo \
  -H "Authorization: Bearer $FASTPROOF_TOKEN" -H 'Content-Type: application/json' \
  -d '{"batchId": "0199aaaa-0000-7000-8000-000000000001"}'
```

## Auto-apply

Settings → Analysis → Auto-apply (off by default). After each launch, clusters nobody decided
whose first suggestion is `same` get its defect type by themselves: no comment, issue or mute,
recorded as source `auto` by `fastproof-analysis`, one batch per launch (undoable), audited as
`suggestion.autoApply`. Automatic decisions are treated like rule decisions: later rules, people
and agents overwrite them. One batch changes at most 20 000 results, so it can always be undone;
larger `same` clusters are left to people. A result whose automatic decision someone undid is
not decided automatically again when the launch is analysed again (its suggestion stays open). It
stays off while replicas of an older version still run (the analysis gate). Check the backtest
first.

## Backtest

The backtest measures how good suggestions would have been in this project: it replays the
decisions people made, in time order, and for each one asks what the memory built from the
decisions before it would have suggested, with the same scoring code as the live suggestions.
The report gives, per band, how many decisions had a suggestion in that band, how many named
the right type and group, the precision, the coverage (decisions with any suggestion) and the
most frequent mistakes.

- **In the UI:** Settings → Analysis → Backtest → **Run backtest** (the last 180 days). The
  latest report shows there; with `backtest.nightly` (on by default) a backtest runs every night
  when there are new decisions.
- **Over the API:** `POST /api/v1/projects/{key}/analysis/backtest {"days": 180}` (needs
  `settings:write`) queues it; `GET …/analysis/backtest/latest` reads the report. A project has
  one backtest queued or running at a time (409 `limit_reached` for another). A backtest replays
  for at most 10 minutes and holds about 512 MiB of memory at most; beyond either, the report
  covers the decisions replayed so far and is marked `truncated`.
- **From the CLI**, with exactly one source:

```sh
# On the server, for a project (waits for the report):
fastproof analysis backtest --project shop --days 180 \
  --url https://fastproof.example.com --token-file token.txt

# Offline, on an export of the decision events (no server needed):
curl -H "Authorization: Bearer $FASTPROOF_TOKEN" \
  "https://fastproof.example.com/api/v1/projects/shop/analysis/decision-events?limit=10000" > events.ndjson
fastproof analysis backtest --events events.ndjson

# Offline, on synthetic data (profiles s, m and drift):
fastproof analysis backtest --synthetic s --json
```

`--json` prints the report; `--min-precision same=0.97,high=0.85` makes the command exit 1 when
a band is below its minimum (for CI). The export (`settings:write`) writes one JSON decision
event per line, with the author replaced by the first 16 hex digits of a sha256 of the author
and a random secret of the project that the API never shows: the same person has the same hash
in every export of the project, and a guessed name cannot be checked against the file. A page
holds at most `limit` events and 8 MiB; when events are left it carries the `after` of the next
page in the `x-fastproof-next-after` header. The offline report of an export equals the server's report for
the same events.

### Running it later on real history

The harness has no code path for synthetic data: the server reads the stored decision events
of a project, which is what a real project has. To measure a real project, let people triage
as usual for a while (or import its ReportPortal history, whose issues become people's
decisions), then run the backtest on the install, or export the events and run
`fastproof analysis backtest --events` anywhere. The synthetic profiles only check the harness
itself: failure families with a known cause (product bugs, DNS and 503 noise, timing and
order-dependent flaky tests, infrastructure bursts, unmatched mock requests and an error whose
message changed form) and a person who is right 90% of the time on a new error.

## Settings

| Setting | Default | Range |
|---|---|---|
| `analysis.enabled` | on | the project's opt-out |
| `analysis.suggest.minScore` | 0.40 | 0.40 to 0.95 |
| `analysis.suggest.maxCandidates` | 3 | 1 to 3 |
| `analysis.autoApply.enabled` | off | `bands` is `["same"]` |
| `analysis.memoryDays` | 365 | 30 to 3650 |
| `analysis.backtest.nightly` | on | |

The instance limits (`FASTPROOF_ANALYSIS_MAX_CLUSTERS`, `FASTPROOF_ANALYSIS_CANDIDATE_ROWS`,
`FASTPROOF_ANALYSIS_MEMORY_MAX_ROWS`, `FASTPROOF_ANALYSIS_RECOMPUTE_SECONDS`,
`FASTPROOF_ANALYSIS_BACKTEST_MAX_EVENTS`) are in [Configuration](../operations/configuration.md).
With the PostgreSQL extension `pg_trgm`, similar messages are fetched by an index (built when the
server starts); without it, or when that search of a launch takes more than 10 seconds, the
newest `FASTPROOF_ANALYSIS_CANDIDATE_ROWS` memory entries are compared. The final score is
computed the same way either way.

## Permissions

| Action | Permission |
|---|---|
| Read suggestions, memory | `launches:read` |
| Accept, reject, recompute, undo | `results:triage` (undo of mutes also `results:mute`) |
| Make a rule | `rules:write` or `ai:propose` |
| Run a backtest, export decision events, change settings | `settings:write` |
| Read the latest backtest | `projects:read` |

## What it is not

- Not a trained or self-learning model, and no data leaves the install: suggestions come from
  this project's own decisions, scored by a fixed table.
- It does not mute, create issues or rules by itself: auto-apply only sets a defect type and is
  off by default.
- It does not replace triage: it shows what people decided for the same error before and why.
