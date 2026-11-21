# Concepts

## Projects

A **project** groups the launches of one product or team and is the unit of access control: roles are bound
per project (or to all projects). A project has a key (`^[a-z0-9][a-z0-9-]{1,62}$`, for example `web` or
`payments-api`), a name and a description. Creating and deleting projects needs the global
`projects:write` permission.

## Launches

A **launch** is one test run: a CI pipeline, a nightly job or a local run. It carries:

- a name and attributes (`key: value` pairs such as `suite: smoke`);
- CI metadata (provider, pipeline and job id, URL, build order), detected by the CLI in GitLab CI, GitHub
  Actions and Jenkins;
- VCS metadata (repository, branch, commit);
- an environment (a key such as `staging` with values such as browser or OS);
- a status: `running`, `finished` or `interrupted`, and counts per result status.

A launch can be started with an **external id** (`FASTPROOF_LAUNCH`, for example the pipeline id). Starting a
launch with an id that already exists joins it instead of creating a second one, so every job of a pipeline
reports into the same launch.

Launches marked `debug` (local runs, experiments) are kept apart from the history and statistics.

## Uploads and shards

A launch receives any number of **uploads**. Each upload is one archive (zip, tar, tar.gz), one file or one
streamed batch of FastProof JSON results. Parallel CI jobs (shards) each send their own upload into the same
launch. Uploads are queued in PostgreSQL and processed by any replica; each has a status (`queued`,
`processing`, `done`, `failed`), counts of results, attachments and ignored entries, and warnings.

Allure containers (fixtures) and their results may arrive in different uploads and in any order; FastProof
attaches fixtures to results as soon as both are present.

## Results, attempts and steps

- A **result** is the outcome of one test in one launch: status (`passed`, `failed`, `broken`, `skipped`,
  `unknown`), duration, labels, parameters, links, description, defect information.
- A result has one or more **attempts**. Retries of the same test in the same launch are attempts of one
  result, not separate results.
- Each attempt has its error (message, trace, expected and actual values), a tree of **steps** (fixtures are
  steps of kind `fixture`), attachments, stdout and stderr, and an optional trace id.
- **Attachments** are stored once per project by content hash (SHA-256): screenshots, videos, Playwright
  traces, image diffs, HTTP exchanges, logs and other files.

## Test identity

To build history, FastProof must recognise the same test in different launches. It uses the same identity as
Allure 2.45+ and Allure 3, so history stays compatible:

1. An explicit stable id wins: the Allure `ALLURE_ID` label or `testCaseId`, the CTRF `testId`.
2. Otherwise a canonical name per format: Allure `fullName`; JUnit `classname#name` with parameters split
   out of `name[...]`; Playwright relative file and title path; CTRF file path, suite and name.
3. `testCaseHash = md5(testCaseId or fullName)`.
4. `parametersHash` covers the non-excluded parameters sorted by name (masked and hidden values count for
   identity but are never shown or returned by the API).
5. A **series** is the test case plus its parameters and environment key. History, flakiness and durations are
   tracked per series. In Allure terms this is the `retryHash`.

Renaming a test or a file changes its canonical name; give tests an explicit id when history matters across
renames.

## Retries and flaky tests

- **Retries inside a launch.** Results of the same series in one launch become attempts of one result; the
  latest by start time (then by upload order) decides the final status. When attempts disagree (failed, then
  passed) the result is **flaky in run**. Explicit retry signals are read where the format has them: Allure
  `statusDetails.flaky` and repeated results, Playwright retries and status `flaky`, Surefire
  `flakyFailure`/`rerunFailure`, CTRF `retryAttempts` and `flaky`.
- **Flaky across launches.** Per project, FastProof follows the Allure 3 algorithm: within a history depth,
  a test that changes between passing and failing is flaky until it has a stabilization period of identical
  outcomes. Skipped and unknown results do not count.
- **Transitions.** Compared with the previous launch of the same branch and environment, a result is
  `new`, `fixed`, `regressed` or `malfunctioned` (failed turned broken or the other way round).

## Failures and triage

Failures are grouped by a **fingerprint**: the error message with numbers, ids and timestamps normalised,
plus the top stack frames. Triage (`results:triage`) sets a defect type (product bug, automation bug, system
issue, no defect, to investigate), a comment and an issue link, or mutes a test until a date; it can be applied
to every result with the same fingerprint. Rules triage known failures automatically. Every change is
written to the audit log. See [Rules and triage](rules-and-triage.md).

## FastMock correlation

When the test or its adapter records a W3C `traceparent` for an attempt (an Allure label or attachment, or the
`traceId` field of the FastProof JSON format), the result page can show the interactions that FastMock
recorded for that trace: the requests the system under test sent to its mocked dependencies.

## Replicas

FastProof keeps no state in process memory between requests. Any number of replicas share PostgreSQL and the
blob store: the ingest queue uses `FOR UPDATE SKIP LOCKED`, so each upload is processed once; live updates
reach all replicas through PostgreSQL `LISTEN/NOTIFY` or, optionally, Valkey.
