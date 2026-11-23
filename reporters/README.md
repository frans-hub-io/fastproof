# FastProof reporters

Thin, dependency-free reporters that send test runs to FastProof while they happen.

| Package | Framework | Directory |
|---|---|---|
| `@franshub/fastproof-playwright` | Playwright Test ≥ 1.45 (tested with 1.65.0), Node ≥ 20 | [`playwright/`](playwright) |
| `pytest-fastproof` | pytest ≥ 7 (tested with 9.2.0), Python ≥ 3.9 (tested on 3.11), xdist and pytest-rerunfailures | [`pytest/`](pytest) |

Allure adapter enhancers keep the Allure adapter and add live streaming, `traceparent`, retries as attempts and
masking on top (see the docs page "Reporters"):

| Package | Adapter | Directory |
|---|---|---|
| `@franshub/fastproof-allure-js` | allure-playwright 3.x, other allure-js-commons adapters | [`allure-js/`](allure-js) |
| `fastproof-allure-java` | allure-java 3.1.x (JUnit 5, TestNG) | [`allure-java/`](allure-java) |
| `fastproof-allure-pytest` | allure-pytest 2.x | [`allure-pytest/`](allure-pytest) |

The wire contract (HTTP calls, ordering, idempotency, retries, spool format) is in [PROTOCOL.md](PROTOCOL.md);
new reporters follow it.

## What they do

- Create or join a launch by external id (from `FASTPROOF_LAUNCH` or the CI: GitLab, GitHub Actions, Jenkins),
  so parallel shards and jobs land in one launch without a pre-created id or a merge step.
- Open each test as `running` when it starts, stream stdout/stderr, log records and steps live, and send the
  final result as soon as the attempt ends.
- Report **every retry as its own attempt** with its own status, error, output, attachments and trace id:
  Playwright retries and pytest-rerunfailures alike; the server marks mixed outcomes as flaky.
- Upload screenshots, videos, Playwright traces and any attached file after the attempt, in the background,
  streamed from disk and deduplicated by sha256.
- Give every attempt a W3C `traceparent` and `baggage` (launch, test, attempt) and expose it to the test, so a
  FastMock journal or a tracing backend can be joined with the attempt.
- Mask secrets before anything leaves the process: values of `*TOKEN*`, `*SECRET*`, `*PASSWORD*`, `*_KEY`…
  variables, configured values and patterns; in names, parameters, errors, output, logs, attachment names and
  text attachments.
- Never fail the run: hooks only queue work; when the server is slow or down the reporter retries with backoff,
  then writes a bounded spool on disk (200 MB by default) that the next run or a `flush` command sends.
- Run only what a `testplan.json` (Allure format) selects.
- No telemetry and no calls anywhere except the configured FastProof server.

## Installing before the first release

The packages are not published on npm and PyPI yet. Install them from this folder:

```bash
pip install ./reporters/pytest

cd reporters/playwright && npm ci && npm run build && npm pack
# then, in the test project:
npm install --save-dev /path/to/fastproof/reporters/playwright/franshub-fastproof-playwright-0.1.0.tgz
```

## Playwright

```bash
npm install --save-dev @franshub/fastproof-playwright
```

```ts
// playwright.config.ts
import { defineConfig } from '@playwright/test';
import { testPlanGrep } from '@franshub/fastproof-playwright';

export default defineConfig({
  retries: 2,
  grep: testPlanGrep(),
  reporter: [['list'], ['@franshub/fastproof-playwright', { secrets: { patterns: ['card-\\d{4}'] } }]],
  use: { trace: 'retain-on-failure-and-retries', screenshot: 'only-on-failure', fastproofPropagate: 'all' },
});
```

```ts
// tests use the extended `test` for trace correlation and test plans
import { test, expect } from '@franshub/fastproof-playwright/fixture';

test('pays', async ({ page, request, fastproofTrace }) => {
  await request.post('/api/pay', { headers: fastproofTrace.headers({ newSpan: true }) });
});
```

- Options: `url`, `project`, `launch`, `launchName`, `environment`, `attributes`, `debug`, `enabled`,
  `offline`, `spoolDir`, `spoolMaxBytes`, `flushTimeoutMs`, `runningResults`, `liveLogs`, `finishLaunch`
  (`auto|always|never`), `secrets {env, values, patterns}`, `attachments`, `maxAttachmentBytes`,
  `uploadHint`, `quiet`. The token comes from `FASTPROOF_TOKEN` or `FASTPROOF_TOKEN_FILE`; see PROTOCOL.md §1
  for all variables.
- Fixture `fastproofTrace`: `traceparent`, `traceId`, `spanId`, `baggage`, `headers({newSpan})`, `env()`.
  Option `fastproofPropagate`: `'off'` (default), `'all'` (adds the headers to `extraHTTPHeaders` of
  page, context and `request`), or a list of origins (browser requests to those origins only).
  `traceHeaders(testInfo)` for `request.newContext()`.
- Test plans: `testPlanGrep()` for title selectors; the auto fixture in `/fixture` skips tests outside the plan
  for every selector form and for ids (`fastproof.id`, `allure_id` annotations).
- `npx fastproof-playwright flush [--spool DIR]` sends what an earlier run spooled (each session to the project
  it was recorded for; `FASTPROOF_PROJECT` limits it to one); `list` shows it.

## pytest

```bash
pip install pytest-fastproof
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_PROJECT=shop FASTPROOF_TOKEN=... pytest -n 4 --reruns 2
```

The plugin loads through the `pytest11` entry point and stays off without `FASTPROOF_URL` (`-p fastproof`
loads it when plugin autoloading is disabled).

```python
import logging
import pytest_fastproof as fastproof

def test_pay(fastproof_trace):
    logging.getLogger("shop").info("paying")              # streamed live
    with fastproof.step("call the payment API"):          # live step + step tree
        requests.post(url, headers=fastproof_trace.headers())
    fastproof.attach(resp.content, name="response.json")  # bytes or text
    fastproof.attach_file("screenshot.png")
    subprocess.run(cmd, env=fastproof_trace.env())        # TRACEPARENT/BAGGAGE for child processes
```

- Options: `--fastproof-url`, `--fastproof-project`, `--fastproof-launch`, `--fastproof-launch-name`,
  `--fastproof-environment`, `--fastproof-attr KEY=VALUE`, `--fastproof-testplan PATH`,
  `--fastproof-spool-dir`, `--fastproof-offline`, `--fastproof-disable`, `--fastproof-finish`,
  `--fastproof-flush-timeout`, `--fastproof-secret-env`, `--fastproof-secret-pattern`; ini `fastproof_url`,
  `fastproof_project`, `fastproof_secret_env`, `fastproof_secret_patterns`, `fastproof_log_level`.
- Markers: `fastproof_id("ID")`, `fastproof_label(name, value)`, `fastproof_link(url, name=, type=)`; other
  custom markers become `tag` labels.
- Status per attempt: setup or teardown errors and non-assertion exceptions are `broken`, assertion failures
  `failed`, skips and xfail `skipped`.
- xdist: only the controller sends results (from the serialized phase reports, so nothing is sent twice);
  workers stream their logs to the same launch by external id; a crashed worker's test is reported `broken`
  instead of disappearing.
- `python -m pytest_fastproof flush [--spool DIR]` sends what an earlier run spooled (each session to the
  project it was recorded for; `FASTPROOF_PROJECT` limits it to one); `list` shows it.
- Use either the plugin or `fastproof upload` for a run: with `FASTPROOF_URL` set, uploading the
  `--alluredir` output too makes a second launch (the plugin prints a reminder; `FASTPROOF_ENABLED=false`
  turns it off for such runs).

## Compared with ReportPortal agents and Allure adapters

Based on the research of ReportPortal agents 5.x, Allure adapters 2.x/3.x and commercial reporters
(November 2026). "RP" are the ReportPortal agents, "Allure" the Allure adapters plus `allurectl watch`.

| Need | RP agents | Allure adapters | FastProof reporters |
|---|---|---|---|
| Live test start, logs and steps during the test | yes for most (not Vitest); Playwright attachments only at the end | no: a file per finished test; `watch` uploads after the test | yes: running result, batched live logs and steps, final result at attempt end |
| Retries as attempts | correct only in TestNG, partly Playwright/Jest; pytest-rerunfailures merged into one item (agent-python-pytest#403); Playwright retries hang or duplicate (#85, #134) | Playwright yes; pytest-rerunfailures not recognised (#815) | every attempt is a result with its own status, output, attachments and trace id; rerunfailures and Playwright retries tested |
| Shards and parallel workers in one launch | a launch per process, then a pre-created `launchId` or a merge | one results directory | same external id from CI joins one launch; xdist controller-only sending, no duplicates |
| Server down | no disk buffer; Jest agent failed pipelines, launches stuck `IN_PROGRESS` | files by nature | retries with backoff, circuit breaker, bounded spool with priority for failures, sent by the next run or `flush` |
| Transport cost | JS sends one request per log line and buffers attachments in memory; pytest client synchronous by default | local files | one background sender per process, NDJSON batches (500 lines / 200 ms), attachments streamed from disk |
| Attachment dedup | no | no (copied byte for byte) | sha256 per launch |
| Trace correlation per attempt | no | no | `traceparent` + `baggage` per attempt, fixture and header helpers, `TRACEPARENT` env for child processes |
| Secret masking at the source | only the Java REST Assured logger | `masked` parameters are written in clear text to `result.json` | env-derived, literal and regex masking of names, parameters, errors, output, logs, attachment names and text attachments |
| Selective runs | no | `testplan.json` | the same `testplan.json` (grep helper + fixture for Playwright, deselection for pytest) |
| Telemetry | Google Analytics event in JS and .NET clients by default | none | none |

Not covered yet: pre-signed direct-to-S3 uploads, a server-side plan endpoint, `fastproof upload --spool` in
the CLI, masking inside Playwright trace archives (HAR), and HTTP-client hooks that inject `traceparent`
automatically in Python (`requests`/`httpx`).

## Development

```bash
cd playwright && npm ci && npm run build && npm run typecheck && npm test
cd pytest && pip install -e . pytest-rerunfailures pytest-xdist && pytest
```

Integration tests run both reporters against a real `fastproof` binary and PostgreSQL:

```bash
cargo build -p fastproof-server
FASTPROOF_TEST_SERVER_BIN=target/debug/fastproof \
FASTPROOF_TEST_DATABASE_URL=postgres://user@127.0.0.1:5432/postgres \
FASTPROOF_TEST_BROWSER=1 pytest -p no:fastproof reporters/integration
```

`FASTPROOF_TEST_BROWSER=1` also runs a Chromium test with a screenshot, a video and a trace (needs
`npx playwright install chromium-headless-shell`). The database user needs trust authentication and the right
to create databases; each run creates and drops its own `fastproof_test_reporters_*` database.
