# Reporters

FastProof accepts the report files your tests already write (see [Report formats](formats.md)). Its own
reporters go further: they send results while tests run, stream logs live, keep every retry as an attempt, and
never fail your test run when the server is unreachable.

| Reporter | Package | Status |
|---|---|---|
| Playwright | `@franshub/fastproof-playwright` (npm) | First release in preparation |
| pytest | `pytest-fastproof` (PyPI) | First release in preparation |

More frameworks follow; until then, upload the files of the framework's own reporter with `fastproof upload`.

!!! note "Installing before the first release"
    The packages are not on npm and PyPI yet. Install them from a checkout of the FastProof repository:

    ```sh
    # pytest
    pip install ./reporters/pytest

    # Playwright: build a package file once, then install it in your test project
    cd reporters/playwright && npm ci && npm run build && npm pack
    npm install --save-dev /path/to/fastproof/reporters/playwright/franshub-fastproof-playwright-0.1.0.tgz
    ```

    The commands below (`npm install @franshub/fastproof-playwright`, `pip install pytest-fastproof`) work once
    the packages are published.

## What they do

- **Live results:** a test shows up as `running` when it starts and gets its status when it ends.
- **Live logs:** stdout, stderr, logging and steps reach the launch page while the test runs (see
  [Live logs](live-logs.md)).
- **Attempts:** each retry is stored as an attempt of the same result, with its own log and attachments.
- **Attachments:** screenshots, videos, traces and attached files are uploaded with the result.
- **Trace correlation:** each attempt carries a W3C `traceparent`, so calls to FastMock and traced services
  can be matched to the test that made them.
- **Secret masking at the source:** values of environment variables that look like secrets (`*TOKEN*`,
  `*SECRET*`, `*PASSWORD*`, `*_KEY` and similar) and your own patterns are masked before anything is sent.
- **Offline spool:** when the server is down or slow, everything goes to a bounded spool on disk (default
  `.fastproof/spool`, 200 MB) and is retried with backoff; the next run sends what is left, or you send it
  yourself. A reporter problem never changes the result of the test run.
- **CI aware:** the launch name and the external launch id come from the CI system, so parallel shards and
  jobs of one pipeline join one launch.
- **Test plans:** an Allure-format `testplan.json` (`FASTPROOF_TESTPLAN`) selects which tests run; `fastproof
  testplan` prints one, for example of the tests that failed in a launch.

## Common settings

Both reporters read the same environment variables as the `fastproof` CLI; without `FASTPROOF_URL` they stay
off.

| Variable | Meaning |
|---|---|
| `FASTPROOF_URL` | Server URL |
| `FASTPROOF_TOKEN`, `FASTPROOF_TOKEN_FILE` | API token with `launches:write` (or a file holding it) |
| `FASTPROOF_PROJECT` | Project key |
| `FASTPROOF_LAUNCH` | External launch id; runs with the same id join one launch (default: from CI) |
| `FASTPROOF_LAUNCH_NAME` | Launch name |
| `FASTPROOF_ENVIRONMENT` | Environment key, part of the test identity |
| `FASTPROOF_ENABLED` | `false` turns the reporter off |
| `FASTPROOF_OFFLINE` | `true` writes only the spool and sends nothing |
| `FASTPROOF_SPOOL_DIR`, `FASTPROOF_SPOOL_MAX_BYTES` | Spool location and size limit |
| `FASTPROOF_FLUSH_TIMEOUT_MS` | How long the end of the run waits for the last uploads (default 30 s) |
| `FASTPROOF_LIVE_LOGS` | `false` sends logs with the result instead of live |
| `FASTPROOF_SECRET_ENV`, `FASTPROOF_SECRET_PATTERNS` | Extra environment variable names (globs) and regular expressions to mask |
| `FASTPROOF_TESTPLAN` | Path of an Allure-format `testplan.json` (`ALLURE_TESTPLAN_PATH` is read too) |
| `FASTPROOF_QUIET` | `true` prints nothing but errors |
| `FASTPROOF_DIRECT_UPLOAD_MODE`, `FASTPROOF_DIRECT_UPLOAD_MIN_BYTES` | `off` sends every attachment through the server; by default attachments of 1 MiB and more go straight to the S3 store when the server offers it ([Direct uploads](direct-uploads.md)) |

Give the token through your CI system's secret store, never in the repository.

!!! warning "Report each run once"
    With a reporter installed and `FASTPROOF_URL` exported, the run is already reported live. Uploading the
    files the same run wrote (`fastproof upload allure-results`, a JUnit or Playwright JSON report) creates a
    second launch with the same tests, which doubles history and flaky statistics. Use either the reporter or
    `fastproof upload`, or turn the reporter off for that run with `FASTPROOF_ENABLED=false`. The pytest plugin
    prints a reminder when it runs together with `--alluredir`.

## Playwright

```sh
npm install --save-dev @franshub/fastproof-playwright
```

```ts
// playwright.config.ts
import { defineConfig } from '@playwright/test';

export default defineConfig({
  retries: 2,
  reporter: [
    ['list'],
    ['@franshub/fastproof-playwright', { environment: 'staging' }],
  ],
});
```

Options in the config override the environment. Playwright 1.45 or newer and Node.js 20 or newer are
supported. Keep your usual reporters: FastProof's does not print the test list. The Playwright retry number
becomes the attempt, and `test.step` steps and failed `expect` calls are streamed as steps. The
`@franshub/fastproof-playwright/fixture` export adds a `test` with trace headers for outgoing requests.

Send what earlier runs left in the spool:

```sh
npx fastproof-playwright flush --spool .fastproof/spool
```

`flush` needs `FASTPROOF_URL` and a token. Each spooled session goes to the project it was recorded for;
`FASTPROOF_PROJECT` limits the flush to one project.

## pytest

```sh
pip install pytest-fastproof
```

```sh
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_PROJECT=shop \
  pytest --fastproof-environment staging
```

The plugin registers itself; settings are also `--fastproof-*` options (`--fastproof-url`,
`--fastproof-project`, `--fastproof-launch`, `--fastproof-offline`, `--fastproof-disable` and others; see
`pytest --help`). pytest-xdist works: workers stream logs, and the controller sends results once. Live
streaming covers `logging` records and steps; captured stdout and stderr arrive per test phase (or live with
`-s`). Records from `INFO` up are streamed (the default of `fastproof_log_level`); change it with
`fastproof_log_level` in the ini file or `FASTPROOF_LOG_LEVEL` (for example `DEBUG`). The plugin lowers the
root logger to that level when it is higher. pytest's own `log_level` is a separate setting: when you set it,
pytest applies it to the root logger during each test, so records below it do not reach the plugin either.

Send what earlier runs left in the spool:

```sh
python -m pytest_fastproof flush
```

As with Playwright, `flush` needs `FASTPROOF_URL` and a token; each session goes to the project it was recorded
for, and `FASTPROOF_PROJECT` limits the flush to one project. Both tools read the same spool format, so either
one can send the other's sessions (`--spool DIR`).

## Allure adapter enhancers

If your tests already use an Allure adapter, keep it. An enhancer plugs into the adapter's own listener hooks
and mirrors every attempt to FastProof while the tests run; `allure-results` are written exactly as before, so
`fastproof upload` and Allure Report keep working. What the enhancers add on top of the adapters:

- live results (`running`, then the final result as soon as the attempt ends) and live steps, which the Allure
  file writers only produce at the end of a test;
- every retry as its own attempt, with its own trace id;
- a W3C `traceparent` per attempt, also written as a `traceparent` label into the Allure result (turn it off
  with `FASTPROOF_ALLURE_TRACE_LABEL=false`), and `baggage`, for FastMock and traced backends;
- secret masking before anything is sent (Allure writes `masked` and `hidden` parameters to the result file in
  clear text; the enhancers send `****` instead);
- the same settings, offline spool and "never fail the run" rules as the other reporters
  ([protocol](https://github.com/frans-hub-io/fastproof/blob/main/reporters/PROTOCOL.md)).

Do not upload the same `allure-results` with `fastproof upload` as well: that makes a second launch.

| Language | Package | Hooks |
|---|---|---|
| JavaScript | `@franshub/fastproof-allure-js` | `listeners` option of `allure-playwright` (tested with 3.15.0) or any adapter on `allure-js-commons` |
| Java | `io.franshub.fastproof:fastproof-allure-java` | ServiceLoader `TestLifecycleListener` of allure-java 3.1.x (JUnit 5 and TestNG tested) |
| Python | `fastproof-allure-pytest` | pluggy hooks of `allure-python-commons` (tested with allure-pytest 2.16.2, pytest-rerunfailures, xdist) |

### JavaScript (allure-playwright)

```ts
// playwright.config.ts
reporter: [
  ['list'],
  ['@franshub/fastproof-allure-js/playwright', {
    resultsDir: 'allure-results',
    fastproof: { environment: 'staging' },
  }],
],
```

Use it in place of `allure-playwright`: it is that reporter with the FastProof listener added, and it takes the
same options. Install `@franshub/fastproof-playwright` next to it (the shared core); the
`@franshub/fastproof-playwright/fixture` export gives tests the trace headers for the attempt. Other adapters
on `allure-js-commons` take the listener directly: `createFastProofListener({ resultsDir })`, pass
`.listener` in their `listeners` option, call `.start()` at the beginning and `await .close()` at the end of the
run. Without Playwright the adapter does not tell which test a step belongs to, so live steps are streamed only
while one test runs at a time; results, attempts and masking do not depend on it.

### Java (JUnit 5, TestNG)

Add the jar to the test classpath next to `allure-junit-platform` or `allure-testng`; no change in the tests:

```xml
<dependency>
  <groupId>io.franshub.fastproof</groupId>
  <artifactId>fastproof-allure-java</artifactId>
  <version>0.1.0</version>
  <scope>test</scope>
</dependency>
```

Settings come from `FASTPROOF_*` environment variables or `fastproof.*` system properties
(`fastproof.url`, `fastproof.project`, `fastproof.launch.name`, ...). The results directory is the one Allure
uses (`allure.results.directory`). The report is closed in a JVM shutdown hook. Several test JVMs that should
share one launch need the same `FASTPROOF_LAUNCH` (CI ids are picked up automatically). In test code
`FastProof.trace().orElseThrow().headers(false)` gives the headers of the running attempt. The adapters decide
what a retry looks like: Surefire reruns with JUnit 5 arrive as failed, then passed attempts; a TestNG
`IRetryAnalyzer` retry is written by allure-testng as `skipped` (a known adapter issue), and FastProof shows it
that way.

### Python (allure-pytest)

```sh
pip install fastproof-allure-pytest
pytest --alluredir=allure-results
```

With `--alluredir` the enhancer takes over from `pytest-fastproof` (installed as its dependency); without it
`pytest-fastproof` reports as before. Every `pytest_runtest_logstart` is one attempt, which covers
pytest-rerunfailures, and pytest-xdist workers send their own results into one launch. In tests,
`fastproof_allure_pytest.trace().headers()` and the `fastproof_trace` fixture give the headers of the attempt.
Allure's own `stdout`, `stderr` and `log` attachments are sent as attachments.

The enhancers depend on internal hooks of the Allure adapters, which Allure may change between minor
versions; the tested versions are listed above. They do not work with Allure for .NET 2.x.

