# Release notes

## Unreleased

- **Allure history import.** Bring the history of Allure 2 reports (`history/history.json` and the trend files) and
  of Allure 3 (the history file) in as past launches that join the tests your live results use, with
  `fastproof import allure-history` or in the project settings. Flaky detection, trends, dashboards and new
  failures see the history from before FastProof. See [Importing Allure history](guide/importing-allure-history.md).
  The release adds migration `0820`.

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
