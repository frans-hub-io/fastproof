# Upgrading

Upgrade one minor version at a time. Migrations are additive and run while the old version keeps serving, so a
rolling update needs no downtime; see [Migrations and upgrades](helm.md#migrations-and-upgrades) for how the
Helm chart runs them. Take a database backup before every upgrade: downgrades are not supported once a newer
version has migrated the schema.

This page lists what needs attention per release.

## Next release: faster Kafka writers

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
before the upgrade. `launches.log_bytes` is no longer written and will be dropped in a later release.

### Kafka compression

FastProof now produces its records with Snappy instead of LZ4: the writers read Snappy batches noticeably
faster, at a few percent more space on the brokers (6 % for log lines, 18 % for results in our tests).
FastProof reads every codec, so records produced before the upgrade are read as before. If anything outside
FastProof consumes the `events` topic (or the ingest topics), make sure its Kafka client has the Snappy
codec; most do, but some need a plugin (for example KafkaJS).

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
