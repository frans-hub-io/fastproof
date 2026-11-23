# Upgrading

Upgrade one minor version at a time. Migrations are additive and run while the old version keeps serving, so a
rolling update needs no downtime; see [Migrations and upgrades](helm.md#migrations-and-upgrades) for how the
Helm chart runs them. Take a database backup before every upgrade: downgrades are not supported once a newer
version has migrated the schema.

This page lists what needs attention per release.

## 0.2.0-dev1: big launches on every writer

A launch with many results used to go through one Kafka partition and one writer, and every
transaction storing results of a launch held the launch row to its end, so a single launch of 100,000
results was stored one batch after another; see [Kafka writer](scaling.md#kafka-writer). What changes:

- **Record keys.** The gateway and the upload workers key `ingest.results` records by launch and test
  (`<launch id>/<series key>`) and `ingest.logs` records by launch and result id, so a launch spreads over
  every partition. The attempts of one test, and the lines of one result, still share a partition and keep
  their order; results of different tests of a launch may now be stored in another order than they were
  sent. Records keyed by the launch id alone (previous versions, producers outside FastProof) are stored as
  before.
- **Launch counters by their change.** Storing results no longer recounts the whole launch each time: the
  counters and result times change by what the transaction changed, and the launch row is taken only for
  that last update. Writers lock result rows in id order before the launch row, the order triage, rule tasks
  and mute expiry now take as well. During the rolling update, writers of the previous version (which take
  the launch row first) and of this version may meet in a deadlock on the same launch; PostgreSQL breaks
  it and the transaction runs again (`fastproof_db_tx_retries_total`).
- **Live notifications** of stored results are sent right after the commit instead of inside the
  transaction, so the commits of writers no longer queue behind PostgreSQL's notification lock.
- **Late results.** Results stored after their launch finished queue the launch for code sync again.
- **Old launches.** Results written into a launch whose monthly partition retention already dropped
  (a reopened manual run) recreate that partition instead of landing in the default partition.
- **Deleting a launch while it is written.** Writers of a launch share an advisory lock of it;
  deleting the launch, closing or reopening its manual run, adding items to it and rolling it up take
  it exclusively first, so they wait only for the writers in flight, not for a burst of writers to end.
- **Topics created again.** The writers remember the record their committed offset follows
  (migration `0801_kafka_offsets_last_record`, two nullable columns on `kafka_offsets`); a consumer that
  finds another record there (the topic was deleted and created again, or the broker lost its data) reads
  the new log from its start (`fastproof_kafka_offset_resets_total`).
- **Uploads of deleted launches.** An upload whose launch is deleted while it is processed stops at once
  (metric `fastproof_ingest_jobs_total{outcome="launch_deleted"}`); attachment content it had stored is
  collected like other unlinked attachments.
- **Metrics.** `fastproof_analysis_rechecked_series_total` gains a `phase` label (`before_lock`, `locked`);
  new histograms `fastproof_analysis_stats_lock_wait_seconds` and `fastproof_analysis_stats_locked_seconds`.

### Log events in batches (optional)

`FASTPROOF_OUTBOX_LOG_EVENTS=batch` makes the writers announce stored log lines with one `log.chunks`
event per launch and write instead of one `log.chunk` event per chunk (format 2: `{"v": 2, "launchId",
"lines", "chunks": [{"id", "resultId", "attempt", "firstSeq", "lastSeq", "lines"}]}`, at most 200 chunks
per event). The sinks of this release read both. The default stays `chunk` for this release: sinks of
earlier versions and consumers of the `events` topic outside FastProof skip events they do not know, so
switch to `batch` only once every sink replica runs this release and outside consumers read `log.chunks`.
Before rolling back to an earlier release (or starting sinks of one) after `batch` was on, set `chunk` again
and wait until the sinks have caught up (`fastproof_sink_lag_seconds`, or the outbox and `events` consumer
lag at 0): an earlier sink skips `log.chunks` events and moves past them, and those log lines never reach
ClickHouse or OpenSearch.

## 0.2.0-dev1: faster launch explorer

The tree of a launch is counted in the database and the explorer keeps the aggregates of finished launches
(see [Launch explorer](../guide/launch-explorer.md#large-launches)). What to know:

- Migration `0770_explorer_perf` adds the nullable columns `results.label_values` (each label name with its
  values) and `results.suite_tree`, `suite_titled` (the group path of the result in the Suites tree). Adding them
  rewrites no rows, but the `alter table` needs an exclusive lock on `results` and all its partitions: while it
  waits behind a long read, new queries on results queue behind it. The migration gives up after 5 seconds of
  waiting and runs again at the next start. It also adds the function `fastproof_label_values`, the tables
  `explorer_gen` and `explorer_cache`, and triggers on `results`:
  - a deferred trigger raises the explorer counter of every launch whose results a transaction changed, once
    per launch when the transaction commits, which invalidates the cached aggregates of that launch. This holds
    for writes by replicas of the previous version during the rolling update and for SQL run by hand, so the
    cache stays correct with mixed versions. The counter row is locked only at commit, after every other lock of
    the transaction. In a bench of 100 000 results no difference in ingest speed showed above the run-to-run
    noise;
  - when a row's labels, suite path or full name change without this version's code (an old replica, manual
    SQL), `label_values` is recomputed and `suite_tree` is cleared, so the explorer computes the suite path from
    the row again.
- The scheduled job `explorer.backfill` (every minute, 30 seconds of work per run, 500 results per transaction)
  walks the results once in id order: it fills `label_values`, `suite_tree`, and `attachment_kinds` of results of launches
  created before migration `0600` (which had none, so the attachment filter missed them). Progress is kept in
  `analysis_state` (`explorer.results.backfill`), so the job resumes after a restart. Metrics:
  `fastproof_explorer_backfill_scanned_total`, `fastproof_explorer_backfill_updated_total`, and the gauges
  `fastproof_explorer_backfill_results_scanned` and `fastproof_explorer_backfill_done` (1 when finished).
- Results that replicas of the previous version store during the rolling update have no `label_values` or
  `suite_tree`, and rows they update lose `suite_tree`; the explorer computes these when it reads them, which is
  slower but gives the same tree. The backfill does not revisit them once it has finished. It rewrites every
  older result once, so expect the results table to grow until autovacuum reuses the space. It locks rows in id
  order, skips rows another transaction holds (retried after the walk) and waits at most half a second for a
  lock, so it does not deadlock with triage or ingest.
- The job `explorer.cache.prune` (hourly) removes cached aggregates older than 7 days.
- `GET .../labels` and `GET .../facets` gain `partial`; `GET .../clusters` and `GET .../logs` take `env`.
- `GET .../launches/{id}/results` now takes a heavy-query slot, like the tree (429 or 503 with `Retry-After`
  when the caller or the server is busy).
- A result with several values of a grouping label is now in the group of each value (it was placed by its first
  value), and an empty label value counts as no value in the filters and the **(none)** groups.

## 0.2.0-dev1: video serving and S3 by default

The content route of attachments now answers byte ranges and conditional requests, and `<video>` elements load
attachments through short-lived signed [media URLs](../reference/api.md#attachment-content-and-media-urls).
What to know:

- Migration `0700_blob_identity` adds the table that records which blob store holds the attachments (see
  [Blob store identity](configuration.md#blob-store-identity)).
- Media URLs are signed with a key that combines a database secret and `FASTPROOF_MEDIA_URL_KEY`. The Helm chart
  creates that secret once (`<release>-fastproof-media-key`) and keeps it; outside Helm, set the same value on
  every replica. Without it each process signs with a key of its own and URLs only work on the replica that issued
  them.
- New settings with defaults: `FASTPROOF_MEDIA_URL_TTL_SECONDS` (300), `FASTPROOF_MEDIA_MAX_STREAMS` (256),
  `FASTPROOF_MEDIA_STREAM_TIMEOUT_SECONDS` (60), `FASTPROOF_MEDIA_KEY_RELOAD_SECONDS` (30).
- Metrics gain `media_urls_issued_total`, `media_url_rejections_total{reason}`, `attachment_bytes_served_total`
  and `attachment_streams_refused_total`.

### Seekable copies of videos

A new background job, `media.prepare`, makes [seekable copies](../reference/api.md#seekable-videos) of WebM
and MP4 attachments that need them, existing ones included: after the upgrade it works through the stored videos a
few at a time, oldest first. Two migrations add the column `attachments.media_state`, the table
`attachment_media` and a partial index built concurrently. Each copy is stored next to its original (under
`projects/<key>/derived/`) and needs as much space again as the videos that get one; recordings that already seek
well get none. The job downloads one video at a time to `FASTPROOF_SPOOL_DIR` and writes the copy there, so the
spool needs twice the size of the largest video (the job takes two of the spool slots that uploads share, and leftover
files of a stopped process are removed after the job's time limit). Set `FASTPROOF_MEDIA_PREPARE=false` to turn it off. Metrics gain
`media_prepare_total{outcome}`, `media_prepare_seconds` and `media_prepare_bytes_total`.

### Attachments default to S3

The Helm chart's attachment storage default changes from a volume (`blob.backend: fs`) to object storage:
with `blob.backend` unset, the chart deploys the bundled Garage and uses it, unless `blob.s3.endpoint` names an
external S3 service. The setting `garage.enabled` is now `true` by default and only has an effect while the
backend is `s3` and no endpoint is set. Setting `blob.backend: fs` keeps the volume, as before; the volume
claim then carries `helm.sh/resource-policy: keep` and `argocd.argoproj.io/sync-options: Prune=false`.

The chart decides the backend of an existing release from the release itself:

- **`helm upgrade` with `blob.backend` unset.** The chart reads `FASTPROOF_BLOB_BACKEND` from the release's
  ConfigMap with `lookup` and keeps `fs` for a release that stores attachments on a volume. The notes then
  suggest adding `blob.backend: fs` to your values.
- **`helm template --is-upgrade` and similar renders without cluster access** cannot read the release; the render
  stops with a message asking for an explicit `blob.backend`.
- **GitOps tools that render with `helm template` without `lookup`** have no view of the release. In such setups
  set `blob.backend` explicitly (`fs`, or `s3` with an endpoint) before updating the chart, and give the bundled
  Garage its keys through `garage.credentials.existingSecret` (a render without `lookup` creates new generated
  keys each time). The server adds a second line of defence: it records the blob store it uses, and a start
  against a different store while attachments exist is refused with a message (see
  [Blob store identity](configuration.md#blob-store-identity)).
- **Existing releases that already use `blob.backend: s3`** with an endpoint or with `garage.enabled: true` are
  unchanged.

To move attachments from a volume to S3, keep `blob.backend: fs` until you have copied the blob directory into
the bucket (the object keys are the same relative paths: `projects/<key>/attachments/<aa>/<sha256>`, for
example `rclone copy /var/lib/fastproof/blobs s3:<bucket>`), then switch to `s3` with the endpoint (or the
bundled Garage) together with `server.blobBackendMigrated: true` for one rollout, and verify a few attachments.
The `fs` volume is kept on uninstall.

## 0.2.0-dev1: test management agents, limits and version retention

- A new built-in role `agent` and permission `ai:propose` for the tokens of AI agents: they triage, draft
  cases and rules, and propose case changes over MCP, but cannot change, review or activate cases over REST
  (see [Agent tokens](../guide/mcp-copilot.md#agent-tokens)). Existing roles do not change; agents that use
  `editor` tokens keep working.
- Muting results needs the new permission `results:mute` (in `editor`; `admin` has every permission). Custom
  roles in `FASTPROOF_ACCESS_FILE` that hold `results:triage` and should keep muting need `results:mute` added.
- Migration `0790_tm_people` adds the table of people the assignee picker lists (members of bound OIDC groups
  who opened test management, with their display names and bound groups). Migration
  `0791_tm_index_cases_ai_drafts` builds an index concurrently for the cap on unreviewed AI drafts.
- New limits with defaults: `FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE` (30 per caller and replica; plan
  launches, run items, plan changes, bulk case changes answer `429 rate_limited` beyond it) and
  `FASTPROOF_TM_MAX_OPEN_PLAN_LAUNCHES` (20 open manual runs started from plans per caller and project). Raise
  them before upgrading if scripts start many plan launches under one token.
- Optional request fields now have defaults: `assign` (`none`) and `includeAutomated` (`false`) of plan
  launches, `mode` (`createOnly`) and `unknownFieldValues` (`skip`) of imports, `untestedTo` (`skipped`), the
  `body` of a case (`POST`: empty; `PUT`: kept), `format` (`zip`) of zip exports.
- Case versions can be pruned per project (`retention.caseVersionsKeep`, `retention.caseVersionsDays`; off by
  default) by the hourly job `cases.versions.prune`.
- Case numbers are taken in a short statement of their own, so parallel creates no longer wait for each other;
  a create that fails after its number was taken leaves a gap.

## 0.2.0-dev1: faster Kafka writers

The writers of Kafka mode (and ingest in `direct` mode) store results and logs with far fewer statements and
locks; see [Kafka writer](scaling.md#kafka-writer). Things to know before upgrading:

### Migrations

| Migration | Does |
|---|---|
| `0101_streaming_log_bytes` | Table `launch_log_bytes`: the log byte budget of a launch (`FASTPROOF_LOG_MAX_BYTES_PER_LAUNCH`) moves off the launch row, so log requests no longer wait for results being stored for the same launch. The table's foreign key briefly locks `launches` against writes; the migration gives up after 5 seconds of waiting for that lock and runs again at the next start |
| `0102_streaming_log_bytes_copy` | Copies the budget used so far from `launches.log_bytes` into the new table |

During the rolling update, replicas of the previous version keep charging `launches.log_bytes` while new
ones charge `launch_log_bytes`, so a launch that receives logs from both may take up to twice its log budget
until the old replicas are gone. Rolling back leaves the previous version counting from what it charged
before the upgrade. `launches.log_bytes` is no longer written; a later release drops it.

### Kafka compression

FastProof now produces its records with Snappy instead of LZ4: the writers read Snappy batches noticeably
faster, at a few percent more space on the brokers (6 % for log lines, 18 % for results in our tests).
FastProof reads every codec, so records produced before the upgrade are read as before. If anything outside
FastProof consumes the `events` topic (or the ingest topics), make sure its Kafka client has the Snappy
codec; most do, but some need a plugin (for example KafkaJS).

Brokers with a read-only root filesystem need a writable `/tmp` for the Snappy codec, or they reject the
records with `UnknownServerError` and uploads get 503. The bundled broker gets an `emptyDir` at `/tmp` from the
chart; give your own brokers one before upgrading (see [Kafka](helm.md#kafka-clickhouse-and-opensearch)).

### Bundled Kafka keeps its data

The bundled broker used to write its log into the data volume of the `apache/kafka` image instead of its
volume claim, so every restart of the broker started from empty topics. The chart now mounts the claim over
that path (`kafka.persistence.mountPath: /var/lib/kafka/data`, log in `kafka.persistence.dataDir:
/var/lib/kafka/data/kraft`). The broker restarts once during the upgrade and starts from empty topics one last
time: wait until the consumer lag (`fastproof_kafka_lag`) is 0 and hold uploads while it restarts. FastProof
creates the topics again by itself, and the consumers read the new topics from the start, so nothing written
after the restart is skipped and no pod has to be restarted.

### PostgreSQL sizing

The bundled PostgreSQL now gets `max_connections` 120, `shared_buffers` 256MB, `effective_cache_size` 768MB and a
memory limit of 1536Mi (from 1Gi), and the chart refuses pool sizes the database cannot carry; see
[PostgreSQL memory and connections](scaling.md#postgresql-memory-and-connections). An upgrade with many pods
or large `database.maxConnections` may now fail with the numbers: lower `database.maxConnections`, or raise
`max_connections` together with the memory limit. The new limit restarts PostgreSQL once and it starts with
the new settings; when you later change only `postgresql.config.*`, restart it yourself (`kubectl rollout
restart statefulset <release>-postgresql`), since the PostgreSQL chart does not restart on a configuration
change.

### Shutdown

A stop takes at most `FASTPROOF_SHUTDOWN_TIMEOUT_SECONDS` (25 s by default; the chart sets
`terminationGracePeriodSeconds` minus 5). Open event streams end at once on `SIGTERM` (browsers reconnect),
so a pod with followers of live logs no longer waits 20 s before it stops.

## Test management release

The release after 0.1.x adds [test cases](../guide/test-management.md),
[test plans and manual runs](../guide/manual-runs.md) and [imports](../guide/importing-test-cases.md).

### Roles gain permissions

Built-in roles get the new permissions automatically, and so does every existing binding and API token with
these roles:

| Role | Gains |
|---|---|
| `viewer` | `cases:read` |
| `uploader` | `cases:read`, `cases:sync` |
| `editor` | `cases:read`, `cases:sync`, `cases:write`, `cases:import`, `plans:write`, `runs:execute` |
| `tester` | New role: `projects:read`, `launches:read`, `cases:read`, `runs:execute` |
| `admin` | unchanged (`*`) |

Check before the upgrade:

- **Editors** can now create and change test cases and plans, import cases and run manual tests in their
  projects. Bind people who should only read to `viewer`, and people who only execute manual runs to `tester`.
- **CI tokens** with `uploader` can now update test cases from code (`cases:sync`), but only for launches of
  trusted branches (`main`, `master` and the CI default branch by default, see
  [Trusted launches](../guide/test-management.md#trusted-launches)). Give tokens of merge request and fork
  pipelines a custom role without `cases:sync`.
- **Custom roles** in the access file get nothing new. Add the permissions you want them to have, for example
  `cases:read` to a read-only role.

### Migrations

The release adds migrations `0500` to `0504`. They only add tables, columns, constraints and indexes, and run
online while the previous version keeps serving:

| Migration | Does |
|---|---|
| `0500_tm_cases` | Tables of cases, folders, fields, versions, keys, attachments and AI suggestions; columns `case_id` and `kind` of the existing test identities; their constraints are added without checking existing rows |
| `0501_tm_runs` | Columns of managed plans and of launches (`test_plan_id`, `manual_state`, manual counters, `created_by_can_sync`), the link from results to run items, and the tables of plan items, run items, step results, the code sync queue and background jobs |
| `0502_tm_index_test_cases_case` | An index built with `CREATE INDEX CONCURRENTLY`, outside a transaction |
| `0503_tm_fk` | Validates the constraints of `0500` and `0501` against existing rows, without blocking writes |
| `0504_tm_index_test_cases_unlinked` | A second concurrent index |

On large installations `0502` to `0504` read the whole table of test identities; plan a few minutes for them.
The previous version runs unchanged against the new schema, which is what makes the rolling update safe.

### Cases for existing tests

Ten minutes after the upgrade (`FASTPROOF_TM_BACKFILL_DELAY`), the scheduler queues a backfill once per
project that has launches from before the upgrade and code sync on: every existing test with results on a trusted branch gets a case under the folder `Automated`,
numbered in the order the tests were first seen, and an existing `ALLURE_ID` becomes the case's key. Tests
seen only on other branches stay unlinked. The job runs in the background on the replicas with the
`scheduler` role and does not slow down ingest; 100 000 tests take less than ten minutes.

- Projects whose main branch is not `main` or `master`: set `caseSync.branches` and run the backfill again,
  see [After an upgrade](../guide/test-management.md#after-an-upgrade-cases-for-existing-tests).
- Projects that should not get cases from code: set `"caseSync": {"enabled": false}` within those ten
  minutes (or start the server with a longer `FASTPROOF_TM_BACKFILL_DELAY`); such projects are skipped for
  good and get a backfill only when someone starts it. Cases already created can be moved to the trash in
  bulk.

### Other changes

- Launches started from a plan or with run items keep their manual runs: `retention.rawDays` and
  `launchesKeep` no longer delete them, and the history rollup keeps them. They are deleted only when
  `retention.manualDays` is set (default `0`, keep forever).
- The limit of 1000 test plans per project now counts only plans made with `fastproof testplan --from-launch`
  and `--test-case`; managed plans do not count.
- New notification event `manualRunClosed`. Closing the manual run of a finished launch sends a second round of
  messages for that launch, see [Notifications](../guide/notifications.md#events).
- The CLI reports the CI default branch (`vcs.defaultBranch`) of GitLab CI and GitHub Actions. Update the CLI in
  your CI images so that `useCiDefaultBranch` can trust it.
- New server settings `FASTPROOF_TM_*` for the limits of test management, all with defaults (see
  [Limits](limits.md#test-management)). The Helm chart has no values for them; set them with `server.extraEnv`
  when needed.
- The API only gains fields: `Launch` has `testPlanId` and `manual`, results have `source`, `caseNumber` and
  `runItemId`, and launch `vcs` has `defaultBranch`.

### Rolling back

Rolling back to 0.1.x is not supported once anyone has started a manual run: the older version does not know
manual runs, and its retention and history rollup would delete those launches with their results. Before any
manual run exists, the 0.1.x binary still works against the migrated database. Otherwise restore the backup
taken before the upgrade.
