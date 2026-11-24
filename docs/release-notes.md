# Release notes

## Unreleased

- **Allure history import.** Bring the history of Allure 2 reports (`history/history.json` and the trend files) and
  of Allure 3 (the history file) in as past launches that join the tests your live results use, with
  `fastproof import allure-history` or in the project settings. Flaky detection, trends, dashboards and new
  failures see the history from before FastProof. See [Importing Allure history](guide/importing-allure-history.md).
  The release adds migration `0820`.
- **Security: the offline spool is verified before it is sent.** The `flush` of the Playwright, Vitest, Jest,
  Cypress and pytest reporters built the path of an attachment from the `sha256` in a spooled session without
  checking it, so a spool planted through a restored CI cache, an artifact or a shared `FASTPROOF_SPOOL_DIR`
  could make the next run upload any local file readable by that run to a launch of the planter's choice. Senders
  now accept only 64 lowercase hex names, never follow links, send an attachment only when its content hashes to
  its name, and skip sessions whose project key is invalid. Update the reporters; no server change is needed.

## 0.2.0-dev2

Published as `docker.io/franshub/fastproof:0.2.0-dev2` for `linux/amd64` and `linux/arm64`; Helm chart
`fastproof` 0.2.0. This is a development release. Read [Upgrading](operations/upgrading.md#020-dev2-faster-first-view-of-large-launches-merge-isolation)
before you upgrade: the release adds migrations, a background index job and drops a column that only releases
before 0.2.0-dev1 still write.

### Reports and launches

- **Allure history import.** Bring the history of Allure 2 reports and Allure 3 history files in as past launches
  joined to the same tests, from the command line (`fastproof import allure-history`), the API or the **History
  import** tab of the project settings. It needs the `settings:write` permission. See
  [Importing Allure history](guide/importing-allure-history.md).
- **Faster Suites tree on large launches.** The tree of a finished launch is built ahead in the background and the
  tests of a suite are read through a new index, so the first view of a launch with hundreds of thousands of
  results opens quickly.
- **Merging does not hold up other projects.** A merge, a manual run or a launch deletion in one project no longer
  waits for a merge running in another one.
- **Long names, messages and ids are stored.** A test with a very long name, failure message or writer id used to
  fail its request with `500`; it is stored now. Names sent to the API are cut to 4 KiB with a warning, and the
  identity of a test, and so its history, stays the same.

### Dashboards and integrations

- **Three new dashboard widgets:** cumulative trend, unique bugs and activity stream. See
  [Dashboards](guide/dashboards.md#widgets).
- **Issue tracker status in batches.** FastProof reads the status of many linked issues in one call where the
  tracker allows it, which keeps large projects under the trackers' rate limits. See
  [Issue trackers](guide/issue-trackers.md).

### Reporters

- **Vitest, Jest and Cypress.** New reporters next to Playwright and pytest, with live results and logs; build them from the repository until their packages are published. See
  [Reporters](guide/reporters.md).

### Upgrading

Back up the database first and follow [Upgrading](operations/upgrading.md). Migrations `0800` and `0810` to `0820`
run while the previous version keeps serving; the new indexes are built in the background by the `analysis.indexes`
job, which runs every 10 minutes, so the full speed-up of large launches comes once it has built them. Upgrade
to this release from 0.2.0-dev1, not from an older one: it drops the column `launches.log_bytes`.

## 0.2.0-dev1

Published as `docker.io/franshub/fastproof:0.2.0-dev1` for `linux/amd64` and `linux/arm64`; Helm chart
`fastproof` 0.2.0. This is a development release. Read [Upgrading](operations/upgrading.md)
before you upgrade: the release adds migrations and new defaults.

### Reports and launches

- **Video with steps on the timeline.** Attachments are served with byte ranges and signed media URLs, videos get
  seekable copies in the background, and each step marks the seek bar of the player. See
  [Concepts](guide/concepts.md).
- **Launch explorer.** Large launches open fast: the tree is counted in the database and finished launches keep
  their aggregates. See [Launch explorer](guide/launch-explorer.md).
- **Export.** A launch exports as a self-contained HTML report, a PDF or an Excel workbook, with the filters of the
  launch page applied. See [Exporting a launch](guide/launch-export.md).
- **Merging launches.** Merge launches into a new one or into one of them, with undo. See
  [Merging launches](guide/merging-launches.md).
- **Direct uploads.** CLI and reporters send screenshots, videos and traces straight to S3 through pre-signed URLs;
  the server verifies every object before it becomes an attachment. See [Direct uploads](guide/direct-uploads.md).
- **Allure adapter enhancers.** Keep your Allure adapter and get live results, live steps, every retry as an
  attempt and secret masking. See [Reporters](guide/reporters.md#allure-adapter-enhancers).

### Triage and integrations

- **Rerun in CI.** Rerun failed or selected tests of a launch through your CI with a test plan. See
  [Rerun from a launch](guide/rerun.md).
- **Dashboards.** Any number of dashboards per project made of widgets over launch counters, series, flaky
  statistics and failure fingerprints. See [Dashboards](guide/dashboards.md).
- **Issue trackers.** Create, link and read back issues in Jira, GitLab, GitHub and Azure DevOps. See
  [Issue trackers](guide/issue-trackers.md).

### Test management

- Hardening of test management: a new `agent` role for AI agents, a separate `results:mute` permission, limits on
  operations that multiply writes, and optional pruning of case versions.

### Performance

- **Kafka throughput.** Writers store a launch's results and logs in parallel on every partition with far fewer
  statements and locks. See [Scaling](operations/scaling.md#kafka-writer).

### Upgrading

Back up the database first and follow [Upgrading](operations/upgrading.md). Migrations are additive and run while
the previous version keeps serving.
