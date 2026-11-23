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
  history of their own.
- A triage decision made by a person (defect type, comment, issue) stays: on the target result first, else
  the one of the result whose attempt wins.
- A new target takes the place of the earliest launch in history (its creation time), the CI and VCS data of
  that launch, the attributes of all of them, the next launch number and, by default, the earliest name.
- Containers (fixtures), attachment links and live log chunks are copied with the attempts.
- The merged launches are **hidden**: they leave the launch list, history, dashboards and gates. Opening one
  shows where it went. Results, uploads, logs and finish requests sent to a hidden launch are refused with
  `409 launch_merged` (with `details.mergedInto`), and starting a launch with the CI id of a hidden launch
  returns the target, so a pipeline that reports again lands in the right place.
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

- a target the merge created is deleted. If results were added to it after the merge, undo is refused
  (`409 merge_target_changed`); delete the launch instead;
- when the merge went into an existing launch, the copied attempts leave it and its results are recomputed
  from its own attempts. Attempts it received after the merge stay.

When the window ends, a scheduled job (`launches.merges`, every minute) deletes the results, attempts, logs
and containers of the hidden launches; their launch rows, uploads and attachment references stay as small
tombstones so that attachments the target shows keep working, and they go when the target is deleted.

A merge **without undo** (`"final": true`, the checkbox in the dialog, `--final --yes` in the CLI) does this
right away and needs `settings:write`, the permission that deletes launches. With
`FASTPROOF_MERGE_UNDO_HOURS=0` every merge is final.

Deleting the target of a merge deletes its hidden launches too (within a minute).

## Rules and limits

- 2 to 20 launches per merge (`FASTPROOF_MERGE_MAX_ATTEMPTS` attempts at most, 200 000 by default;
  `413 merge_too_large` above).
- The launches to merge must be finished and have no upload in progress (`409 launch_running`,
  `409 launch_busy`). The target may still run: ingest into it and the merge wait for each other, and
  results that arrive after the merge become further attempts.
- Same branch, and all debug or all regular (`409 merge_mixed`); launches with a manual run
  (`409 launch_manual`) and launches already in the daily rollup (`409 launch_rolled_up`) are not merged.
- Attempts that reach a merged launch after the merge (sent before it, written after it, e.g. by the Kafka
  writer) are copied into the target by the scheduled job within a minute and once more before the merged
  launches are deleted (`counts.lateAttempts`).
- Repeating a merge of the same launches into the same target is safe: it returns the existing merge
  (`200`) and copies only what is new.

## Permissions

| Action | Needs |
| --- | --- |
| Merge launches you created (a CI token merging its shards) | `launches:write` |
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

Launches are named by id or number (`41` or `#41`). The commands take `--url`, `--token-file` and
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

A merge has `status` `active` (undo possible until `undoUntil`), `undone`, `final` or `deleted` (its target
was deleted), the target (`targetId`, `targetNumber`, `createdTarget`), the `sources` and `counts`
(`attempts`, `duplicates`, `rekeyed`, `results`, `sharedSeries`, `lateAttempts`). Launches carry
`mergedInto` once merged. Outbox events: `launch.merged`, `launch.unmerged`, `launch.merge_finalized`, and
`launch.created` / `launch.finished` for a new target.

## Limitations

- Results of launches created between the merged launches and the merge keep the transitions they got
  when they were uploaded (they saw the merged launches as history).
- Logs indexed in an external search backend (OpenSearch, ClickHouse) stay under the merged launches; the
  log search of the target finds them in PostgreSQL only.
- Undoing a merge into an existing launch keeps the attachment kinds and names its results took from the
  merged launches.
