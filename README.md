# FastProof

FastProof is an open-source (Apache-2.0), self-hosted test reporting server, with test management to follow:
it ingests allure-results, JUnit XML, CTRF and Playwright JSON reports from CI, keeps history per test with
Allure-compatible identity, detects flaky tests and helps triage failures. This repository is at an early
stage; the server, storage and UI are being built now. Workspace crates:

- `crates/fastproof-model`: the normalized result model (launch metadata, results, steps, attachments,
  containers, categories), Allure 2.45+/Allure 3 compatible test identity hashes, retry grouping inside a
  launch and flaky/transition detection across launches.
- `crates/fastproof-ingest`: bounded, streaming parsers for allure-results, JUnit XML (generic, Surefire,
  Playwright, pytest, Vitest, gotestsum), CTRF and the Playwright JSON reporter, reading a single file, a
  directory, a zip, a tar or a tar.gz upload.

Test fixtures produced by the real writers, and the projects that generate them, are in `tests/fixtures`.
