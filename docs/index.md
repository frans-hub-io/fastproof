---
hide:
  - navigation
---

<div class="fp-hero" markdown>

<p class="fp-hero__eyebrow">FastProof by FransHub</p>

# FastProof documentation

FastProof is an open-source (Apache-2.0), self-hosted test reporting server. Your CI uploads the reports it
already writes (allure-results, JUnit XML, CTRF, Playwright JSON or the FastProof JSON format), and FastProof
keeps every launch with its history per test, finds flaky tests, groups failures and helps the team triage
them. One Rust binary serves the API, the ingest queue and the web UI; it needs PostgreSQL and either an
S3-compatible bucket or a directory.

<div class="fp-hero__actions" markdown>
[Get started :material-arrow-right:](guide/getting-started.md){ .md-button .md-button--primary }
[Concepts](guide/concepts.md){ .md-button }
[API reference](reference/api.md){ .md-button }
</div>

</div>

## Why FastProof

<div class="grid cards fp-cards" markdown>

-   :material-feather:{ .fp-card__icon } **Light**

    One static binary in a distroless image, PostgreSQL and object storage. No Elasticsearch, no message broker,
    no JVM. Any number of replicas share the work through PostgreSQL; Valkey is optional.

-   :material-lock-open-variant-outline:{ .fp-card__icon } **Open**

    Apache-2.0, self-hosted, no per-seat licence and no feature gates. Formats in and out are documented,
    including the FastProof JSON report schema.

-   :material-swap-horizontal:{ .fp-card__icon } **Allure-compatible**

    Test identity follows Allure 2.45+ and Allure 3 (`testCaseId`, `fullName`, parameter hashes), so your
    tests keep their history when you move from Allure Report, and Allure adapters work unchanged.

-   :material-link-variant:{ .fp-card__icon } **FastMock correlation**

    When tests run against [FastMock](https://fastmock.franshub.io/), the `traceparent` of each attempt links
    the result to the mock interactions it caused, so a failure shows what the system under test really sent.

</div>

## Start here

<div class="grid cards fp-cards" markdown>

-   :material-rocket-launch-outline:{ .fp-card__icon } **[Getting started](guide/getting-started.md)**

    Run FastProof with Docker or Docker Compose, create a project and upload the first report.

-   :material-lightbulb-on-outline:{ .fp-card__icon } **[Concepts](guide/concepts.md)**

    Projects, launches, shards, test identity, retries and flaky tests.

-   :material-pipe:{ .fp-card__icon } **[CI recipes](guide/ci-recipes.md)**

    GitLab CI, GitHub Actions and Jenkins with `fastproof upload` and `fastproof watch`.

-   :material-file-document-multiple-outline:{ .fp-card__icon } **[Report formats](guide/formats.md)**

    allure-results, JUnit XML variants, CTRF, Playwright JSON and the FastProof JSON report.

-   :material-access-point:{ .fp-card__icon } **[Reporters](guide/reporters.md)**

    Playwright and pytest reporters with live results, live logs and an offline spool.

-   :material-text-box-search-outline:{ .fp-card__icon } **[Live logs](guide/live-logs.md)**

    Follow test output while tests run, and search logs across launches.

-   :material-import:{ .fp-card__icon } **[Migrating from Allure Report](guide/migrating-from-allure.md)**

    Keep your tests and adapters; point the upload at FastProof.

</div>

## Triage and integrations

<div class="grid cards fp-cards" markdown>

-   :material-bug-check-outline:{ .fp-card__icon } **[Rules and triage](guide/rules-and-triage.md)**

    Failure clusters, defect types, mutes and rules that triage known failures on ingest.

-   :material-traffic-light-outline:{ .fp-card__icon } **[Quality gates](guide/quality-gates.md)**

    Pass or fail a launch on its numbers, in CI and on the merge request.

-   :material-bell-ring-outline:{ .fp-card__icon } **[Notifications](guide/notifications.md)**

    Signed webhooks, Slack, Microsoft Teams, email, Telegram, merge request comments and commit statuses.

-   :material-robot-outline:{ .fp-card__icon } **[MCP and GitHub Copilot](guide/mcp-copilot.md)**

    Let a coding agent in your editor investigate and triage failures.

</div>

## Operate

<div class="grid cards fp-cards" markdown>

-   :material-tune-variant:{ .fp-card__icon } **[Configuration](operations/configuration.md)**

    Every environment variable of the server and the CLI.

-   :material-ship-wheel:{ .fp-card__icon } **[Helm chart](operations/helm.md)**

    Values, PostgreSQL and S3 options, upgrades and running several replicas.

-   :material-chart-timeline-variant:{ .fp-card__icon } **[Scaling and big data](operations/scaling.md)**

    Kafka ingest, separate roles, ClickHouse and OpenSearch sinks, sizing.

-   :material-shield-check-outline:{ .fp-card__icon } **[Security](operations/security.md)**

    Authentication, roles and permissions, attachment sandboxing, reporting vulnerabilities.

-   :material-gauge:{ .fp-card__icon } **[Limits](operations/limits.md)**

    Size, depth and count limits applied to every upload.

-   :material-map-outline:{ .fp-card__icon } **[Roadmap](roadmap.md)**

    Test management, ReportPortal agent compatibility, more reporters, FastProof Analysis.

</div>

## Conventions in these pages

- FastProof listens on `http://localhost:8080` (UI, API, sign-in) and `http://localhost:9090` (metrics,
  health and readiness). In Kubernetes both are ports of the chart's Service (`fastproof` for a release named
  `fastproof`).
- `FASTPROOF_TOKEN` holds an API token; `FASTPROOF_URL` the server URL; `FASTPROOF_PROJECT` the project key.
- Example host names use `example.com`, example addresses `198.51.100.0/24` and `203.0.113.0/24`.
- FastProof is at an early stage (`0.1.0-dev1`); names on these pages are the ones the first release ships.
