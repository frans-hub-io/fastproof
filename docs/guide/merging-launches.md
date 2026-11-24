# Merging launches

Parallel CI jobs that share a CI id (or `--launch`) already report into one launch. When they did not, for
example shards of different pipelines, a job re-run by hand, or a nightly that was restarted, merge the
launches afterwards: into a **new launch**, or **into one of them** (a re-run joins the original).

## What a merge does

- Every attempt of the merged launches is copied into the target. A test that ran in more than one of them
  becomes **one result with more attempts**, exactly as retries inside one launch: the latest attempt (by
  start time, then by order of arrival) is the result, and when the attempts disagree (failed, then passed)
  the result is **flaky in run**.
- The counters of the target, the transitions (`new`, `fixed`, `regressed`), cross-run flakiness, default
  and rule defects, mutes and the series statistics (the status string behind history and the flaky list)
  are recomputed as if the target had been uploaded in one go. The merged launches no longer count as
  history of their own. The copy runs in one transaction that takes only the rows of the merged launches;
  the analysis and statistics follow in short transactions of a few hundred tests each, in the lock order
  of ingest, so uploads into other launches with the same tests do not wait for a merge. Until they are
  done, the merge reports `analysing: true`.
- A triage decision made by a person (defect type, comment, issue) stays: on the target result first, else
  the one of the result whose attempt wins.
- A new target takes the place of the earliest launch in history (its creation time), the CI and VCS data of
  that launch, the attributes of all of them, the next launch number and, by default, the earliest name.
- Containers (fixtures), attachment links and live log chunks are copied with the attempts; log lines of an
  attempt whose id was renamed (see below) follow it.
- Code sync trusts the target only if every merged launch and the person merging may sync cases
  (`cases:sync`); otherwise the target is marked untrusted, as a launch that an untrusted caller added
  results to.
- The merged launches are **hidden**: they leave the launch list, history, dashboards and gates. Opening one
  shows where it went. Results, uploads, logs, attachments, triage, changes, finish and delete requests for
  a hidden launch are refused with `409 launch_merged` (with `details.mergedInto`), and starting a launch
  with the CI id of a hidden launch returns the target, so a pipeline that reports again lands in the right
  place. Hidden launches do not count for retention (`launchesKeep`) or the project's launch count; they
  are deleted with their target.
- The quality gate of the target is evaluated again.

Two attempts with the same client id (the `id` reporters send, or the Allure `uuid`) are the same attempt
only when they are of the same test with the same start; otherwise the copy gets a key prefixed with its
source launch, and the merge reports how many attempts were renamed (`rekeyed`) or were already there
(`duplicates`).

## From the UI

Tick the launches in the launch list (people who may upload see the boxes), then **Merge…**. Choose a new
launch (and its name) or one of the selected launches as the target. The dialog checks the launches and
shows how many attempts and tests it will merge and how many tests ran in more than one of them before you
confirm. The target's page then says where it was merged from and offers **Undo merge** while that is
possible.

## Undo, or a merge without undo

By default the merged launches stay hidden for `FASTPROOF_MERGE_UNDO_HOURS` (24 hours). Until then **Undo**
brings them back exactly as they were:

- a target the merge created is deleted. If results or logs were added to it after the merge, undo is
  refused (`409 merge_target_changed`); delete the launch instead. If people triaged its results after the
  merge, undo deletes that work and needs `results:triage` (`403 merge_target_triaged`);
- when the merge went into an existing launch, the copied attempts leave it and its results are recomputed
  from its own attempts. Attempts it received after the merge stay, and so does a running attempt it
  completed itself; a triage decision the merge copied from a merged launch leaves with it, and failure
  fingerprints point where they did before.
- if one of the merged launches was deleted meanwhile, undo is refused (`409 merge_source_gone`).

When the window ends, a scheduled job (`launches.merges`, every minute) deletes the results, attempts, logs
and containers of the hidden launches; their attachment references move to the target, and their launch
rows and uploads stay as small tombstones until the target is deleted. Anything that still reaches a
tombstone during the next days (a Kafka writer far behind) is moved to the target as well.

A merge **without undo** (`"final": true`, the checkbox in the dialog, `--final --yes` in the CLI) cannot be
undone and needs `settings:write`, the permission that deletes launches. Its merged launches are deleted
about two minutes later, after writes already on their way were copied. With
`FASTPROOF_MERGE_UNDO_HOURS=0` every merge is final.

Deleting the target of a merge deletes its hidden launches too (within a minute); the merge becomes
`deleted`.

## Rules and limits

- 2 to 20 launches per merge (`FASTPROOF_MERGE_MAX_ATTEMPTS` attempts at most, 200 000 by default), and at
  most `FASTPROOF_LOG_MAX_BYTES_PER_LAUNCH` log bytes in all, so the target stays within the log budget
  (`413 merge_too_large`, `details.limit` `attempts` or `logBytes`).
- One merge, undo or finalization per project at a time: another one gets `409 merge_busy` at once instead
  of waiting. Merges, undos and previews count as heavy requests of the caller (`429` beyond
  `FASTPROOF_HEAVY_QUERIES_PER_CALLER`). A preview (`dryRun`) takes no locks.
- Merges of different projects do not wait for each other. The analysis that a merge leaves to the
  scheduled job runs in turns (one merge per project, then the next), and a merge whose launches are busy
  (another merge or a manual run of the same project holds them) is left for the job's next run, a minute
  later. Each run starts with another project, and after three busy runs in a row the job waits for that
  merge's launches.
- The launches to merge must be finished and have no upload in progress (`409 launch_running`,
  `409 launch_busy`). The target may still run: ingest into it and the merge wait for each other, and
  results that arrive after the merge become further attempts.
- Same branch, and all debug or all regular (`409 merge_mixed`); launches with a manual run
  (`409 launch_manual`), launches started from a test plan (`409 launch_plan`) and launches already in the
  daily rollup (`409 launch_rolled_up`) are not merged.
- A finished target takes a merge only from its creator or a caller with `cases:sync`
  (`409 launch_finished`), as it takes results.
- Attempts and log lines that reach a merged launch after the merge (sent before it, written after it, e.g.
  by the Kafka writer) are copied into the target by the scheduled job within a minute, once more before the
  merged launches are deleted, and for seven days after that (`counts.lateAttempts`).
- Repeating a merge of the same launches into the same target is safe: it returns the existing merge
  (`200`) and copies only what is new.

## Permissions

| Action | Needs |
| --- | --- |
| Merge launches you created (a CI token merging its shards; the same token or person, not just the same name) | `launches:write` |
| Merge launches others created | `launches:write` and `results:triage` (editor) |
| Merge without undo | additionally `settings:write` |
| Undo | `launches:write`, and `results:triage` unless you made the merge |
| See merges | `launches:read` |

Merges and undos are written to the audit log (`launch.merge`, `launch.merge.undo`).

## CLI

```bash
# preview, then merge launches #41 and #42 into a new launch
fastproof launch merge 41 42 --dry-run
fastproof launch merge 41 42 --name "nightly 2026-11-23"

# merge the re-run #43 into the original #41
fastproof launch merge 43 --into 41

# undo (the merge id is printed by `launch merge`)
fastproof launch undo-merge 0193f3c2-0000-7000-8000-000000000000

# merge without undo
fastproof launch merge 41 42 --final --yes
```

Launches are named by id or number (`41` or `#41`); the number of a merged launch resolves
to its target. The commands take `--url`, `--token-file` and
`--project` like the others.

### A re-run that joins the original launch

Tell the CLI which launch a re-run belongs to, and its results go straight into it instead of a new launch:

```bash
fastproof upload allure-results --rerun-of 41 --wait
# or in CI: FASTPROOF_RERUN_OF=41 fastproof upload allure-results
```

Each test that runs again becomes a new attempt of its result in launch #41, and the result follows the
latest attempt. `--rerun-of` works for `upload`, `watch` and `launch finish`. The launch must accept the
caller: a finished launch takes results from its creator or from a token with `cases:sync` (CI tokens).

## API

| Method and path | |
| --- | --- |
| `POST /api/v1/projects/{key}/launches/merge` | `{"launchIds": [...], "into": id?, "name": "..."?, "final": false, "dryRun": false}`; `201` with the merge, `200` for a dry run (`status: preview`) or a repeated merge |
| `GET /api/v1/projects/{key}/merges/{id}` | One merge |
| `GET /api/v1/projects/{key}/launches/{id}/merges` | Merges the launch took part in (target or source), newest first |
| `POST /api/v1/projects/{key}/merges/{id}/undo` | Undo within the window |
| `GET /api/v1/projects/{key}/launches?includeMerged=true` | The launch list with merged (hidden) launches too |

A merge has `status` `active` (undo possible until `undoUntil`), `undone`, `final` or `deleted` (its target
was deleted), the target (`targetId`, `targetNumber`, `createdTarget`), the `sources` and `counts`
(`attempts`, `duplicates`, `rekeyed`, `results`, `sharedSeries`, `lateAttempts`), and `analysing` while the
analysis of the target is still running. Launches carry
`mergedInto` once merged. Outbox events: `launch.merged`, `launch.unmerged`, `launch.merge_finalized`, and
`launch.created` / `launch.finished` for a new target.

## Limitations

- Results of launches created between the merged launches and the merge keep the transitions they got
  when they were uploaded (they saw the merged launches as history).
- Logs indexed in an external search backend (OpenSearch, ClickHouse) stay under the merged launches; the
  log search of the target finds them in PostgreSQL only.
- Undoing a merge into an existing launch keeps the attachment kinds and names its results took from the
  merged launches.
