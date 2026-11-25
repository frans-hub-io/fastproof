# Release notes

## Unreleased

- **Fixes from the lab smoke.** The exported HTML report stacks each result as a card on a phone (it was a
  table with one character per line in the Suite column; the print layout is unchanged). `fastproof upload --wait`
  prints the launch counts only after the results are stored (it waits up to `--timeout` and says so when the
  counts are not final). `fastproof export --format pdf` names the browser it ran, its exit status and the last
  lines of its error output, and says how to pass a working one. Dashboard widgets no longer leave an empty gap
  below short cards on a phone.
- **Security: reporters verify the offline spool before sending it.** The `flush` of the Playwright, Vitest, Jest,
  Cypress and pytest reporters (and of the new JUnit 5, TestNG, NUnit and xUnit reporters) sends only attachments
  whose content matches their recorded hash, never follows links in the spool directory and skips sessions with
  an invalid project key. Update the reporters; no server change is needed.
- **ReportPortal agents report to FastProof.** The official ReportPortal agents (Java JUnit 5 and TestNG, pytest,
  Robot Framework, Playwright and the others built on the ReportPortal clients) report into a FastProof project
  without changes to the tests: point their endpoint at `https://<your FastProof>/rp`, set the project and use a
  FastProof token with the `uploader` role as the API key. Both API versions (`v1`, `v2`) work. Test items become
  results, retries become attempts of one result (a test that passes on retry is flaky, not two tests), nested
  steps become steps, log lines become live logs, files become attachments, and Playwright traces open in the
  trace viewer. Failure messages come from the ERROR log lines, also when the agent sends them shortly after the
  launch finish. Shards and parallel jobs can report into one launch started with `fastproof launch start`.
  ReportPortal project names map to FastProof projects through the `rp.aliases` project setting. An `issue` sent by
  the agent becomes triage when the token may triage, otherwise a label; its external issues become issue links.
  The endpoint is on by default and is tuned with the `FASTPROOF_RP_*` settings (Helm values `server.rp.*`). Only
  reporting is covered: dashboards, the analyzer, Quality Gates and the rest of the ReportPortal UI API answer
  `501`. See [Migrating from ReportPortal](guide/migrating-from-reportportal.md). The release adds migration
  `0830`.

## 0.2.0-dev2

Published as `docker.io/franshub/fastproof:0.2.0-dev2` for `linux/amd64` and `linux/arm64`; Helm chart
`fastproof` 0.2.0. This is a development release. Read [Upgrading](operations/upgrading.md#020-dev2-faster-first-view-of-large-launches-merge-isolation)
before you upgrade: the release adds migrations, a background index job and drops a column that only releases
before 0.2.0-dev1 still write.

### Reports and launches

- **Import from ReportPortal.** `fastproof rp import` reads launches from a ReportPortal server through its API
  (read-only; the API key comes from `RP_API_KEY` or a file) and replays them into a FastProof project with their
  original ids and times, so the imported history and new reports from the same agents form one series per test.
  By default it takes the last 90 days, log lines at WARN and above and attachments of failed tests up to 64 MiB;
  `--dry-run` counts what would be imported, and an interrupted import continues from its state file. See
  [Migrating from ReportPortal](guide/migrating-from-reportportal.md).
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
