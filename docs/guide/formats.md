# Report formats

FastProof reads the reports your tools already write. The format is detected from file names and content
(`format=auto`); the API and the CLI also accept an explicit format: `allure`, `junit`, `ctrf`, `playwright`.

An upload is one file, a directory (the CLI packs it), or an archive: zip, tar or tar.gz. Archives may mix
formats; unknown files are counted as ignored and reported in the upload's warnings.

## allure-results

The richest format and the recommended one. Written by the Allure adapters for Java (JUnit 5, TestNG,
Cucumber), Python (pytest, behave), JavaScript (Playwright, Jest, Mocha, Cypress, Vitest), .NET, Go, PHP and
more, in both Allure 2 and Allure 3 versions.

| File | Used for |
|---|---|
| `*-result.json` | results: status, timing, steps, parameters, labels, links, attachments |
| `*-container.json` | fixtures (`befores`, `afters`) attached to their results |
| `*-attachment.*` | attachment bodies referenced by results and steps |
| `environment.properties`, `executor.json`, globals | launch environment and executor |
| `categories.json` | failure categories |

Details:

- Test identity uses `testCaseId` or `fullName` and parameter hashes exactly as Allure does, see
  [Concepts](concepts.md#test-identity).
- Masked and hidden parameter values count for identity but are never shown.
- `descriptionHtml` is sanitized; scripts and event handlers are removed.
- Results with `stage` other than `finished`, or without `stop`, are `interrupted`.
- Attachment types are recognised for viewers: images, video, Playwright traces, Allure `image-diff`,
  HTTP exchanges (`application/vnd.allure.http+json`), text and logs, JSON.
- Unknown fields are tolerated.

## JUnit XML

There is no single JUnit XML specification; FastProof reads the common structure (`<testsuites>` or
`<testsuite>` roots, `<testcase>` with `failure`, `error`, `skipped`, `system-out`, `system-err`,
`properties`) and the variants of these producers:

| Producer | What is read |
|---|---|
| Maven Surefire / Failsafe, Gradle | `flakyFailure`/`flakyError` (passed on rerun, becomes a flaky result with attempts) and `rerunFailure`/`rerunError` (failed every time) |
| Playwright JUnit reporter | annotations as properties, `[[ATTACHMENT\|path]]` references (Surefire-style flaky and rerun elements too, but Playwright 1.65 does not write them) |
| pytest (`xunit1`, `xunit2`, `legacy`) | `skipped type="pytest.skip"`/`"pytest.xfail"`, setup and teardown errors, `file` and `line` |
| Vitest | file-based `classname`, unhandled errors as a synthetic test case |
| gotestsum | one suite per package, `--rerun-fails` duplicates as attempts |
| generic | any other writer following the common structure |

The canonical name is `classname#name`; parameters in names such as `test[chrome]` or `method(Type)[1]` are
split out as parameters. XML is parsed without DTDs or external entities and with depth and attribute limits.

!!! note "Playwright retries and JUnit"
    Playwright's JUnit reporter writes one `<testcase>` per test and no `flakyFailure` or `rerunFailure`
    elements (checked with Playwright 1.65), so a test that passed on retry looks passed, and the attachments of
    all its runs land on one attempt. To keep Playwright retries as attempts, upload the JSON report, a CTRF or
    `allure-playwright` report, or use the [FastProof reporter](reporters.md).

## CTRF

[Common Test Report Format](https://ctrf.io/) JSON, spec 0.2.x (older 0.0.x reports are read leniently).
Used: `results.tests[]` with `status`, `duration`, `message`, `trace`, `suite`, `filePath`, `tags`, `labels`,
`parameters`, `steps`, `attachments`, `stdout`/`stderr`; retries from `retryAttempts` and `flaky`; identity
from `testId` when present; launch metadata from `environment` (branch, commit, build). Attachments are paths,
so upload them in the same archive as the report.

## Playwright JSON

The output of `--reporter=json`: `suites → specs → tests → results`. Each Playwright retry becomes an attempt;
status `flaky` and `expectedStatus` are honoured; nested steps, stdout, stderr and attachments (screenshots,
videos, traces, `*-expected.png`/`*-actual.png`/`*-diff.png`) are read. The Playwright project becomes part of
the environment key, so the same test in Chromium and WebKit has two series. Playwright colours its error
messages with ANSI escape codes; FastProof removes them from messages and traces (see
[below](#error-text-and-ansi-codes)).

For sharded runs, upload each shard's JSON into the same launch, or merge blob reports first with
`npx playwright merge-reports --reporter json`. Blob reports themselves are not read.

## FastProof JSON report

FastProof's own format, used by `fastproof watch`, by the streaming API and, later, by FastProof reporters
and launch export. Each `ReportResult` object describes one test execution; the format is versioned by a
JSON Schema:

- Schema: [`schemas/fastproof-report-v1.json`](../schemas/fastproof-report-v1.json), generated from the
  server's data model and published with every release.
- Endpoint: `POST /api/v1/projects/{key}/launches/{id}/results` with one result or a JSON array
  (`application/json`), or one result per line (`application/x-ndjson`) for long-running streams.
- Every result carries a client `id`; sending the same id twice is idempotent (the response counts
  `accepted`, `duplicates` and `rejected` with the index and reason of each rejected entry). Names and
  full names longer than `max_name_bytes` (4 KiB) are cut and listed in `warnings` (index and message).
- Attachments are uploaded first (`POST .../launches/{id}/attachments`, raw body with `Content-Type` and
  `X-FastProof-Attachment-Name`) and referenced by the returned attachment id.
- `traceId` links an attempt to the matching FastMock interactions.

JUnit XML stays an import-only compatibility format; new integrations should write FastProof JSON or
allure-results.

In live reporting a result can first be sent with `status: "running"` and completed later by a line with the
same client `id`. The client id wins: if the final line has another identity than the running one (for
example it adds `testCaseId` or `parameters`), the attempt moves to the result of the final identity and the
running result is removed when it is left empty. A running line completed in the same request counts as
`accepted`, not as a duplicate. See [Live logs](live-logs.md) and the reporter protocol.

## Error text and ANSI codes

Error messages, traces, expected and actual values and step errors are stored without ANSI escape codes
(colours, cursor moves, terminal links), whatever the format or the API they came through. Lists, search,
failure fingerprints, clusters and similar failures therefore see plain text. Captured output (stdout, stderr
and live logs) keeps the codes, and the UI renders them as colours.

## What is not read yet

Cucumber JSON and messages, JUnit Open Test Reporting, TRX, xUnit v2 and OpenTelemetry test spans are planned.
The ReportPortal agent protocol is on the [roadmap](../roadmap.md).
