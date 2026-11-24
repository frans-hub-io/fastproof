# Reporters

FastProof accepts the report files your tests already write (see [Report formats](formats.md)). Its own
reporters go further: they send results while tests run, stream logs live, keep every retry as an attempt, and
never fail your test run when the server is unreachable.

| Reporter | Package | Status |
|---|---|---|
| Playwright | `@franshub/fastproof-playwright` (npm) | First release in preparation |
| pytest | `pytest-fastproof` (PyPI) | First release in preparation |
| Vitest | `@franshub/fastproof-vitest` (npm) | In preparation |
| Jest | `@franshub/fastproof-jest` (npm) | In preparation |
| Cypress | `@franshub/fastproof-cypress` (npm) | In preparation |
| NUnit 4, xUnit.net v3 | `FransHub.FastProof.NUnit`, `FransHub.FastProof.Xunit` (NuGet) | In preparation |
| JUnit 5 | `io.franshub:fastproof-junit5` (Maven) | In preparation |
| TestNG | `io.franshub:fastproof-testng` (Maven) | In preparation |

More frameworks follow; until then, upload the files of the framework's own reporter with `fastproof upload`.

!!! note "Installing before the first release"
    The packages are not on npm and PyPI yet. Install them from a checkout of the FastProof repository:

    ```sh
    # pytest
    pip install ./reporters/pytest

    # Playwright: build a package file once, then install it in your test project
    cd reporters/playwright && npm ci && npm run build && npm pack
    npm install --save-dev /path/to/fastproof/reporters/playwright/franshub-fastproof-playwright-0.1.0.tgz

    # Vitest, Jest, Cypress: same steps in their folders; they need the Playwright package above as well,
    # because they share its core
    cd reporters/vitest && npm install --legacy-peer-deps && npm run build && npm pack

    # .NET: build the packages into a local folder and add it as a NuGet source
    cd reporters/dotnet && dotnet pack src/FransHub.FastProof.NUnit -c Release -o /path/to/nupkgs
    dotnet pack src/FransHub.FastProof.Xunit -c Release -o /path/to/nupkgs
    dotnet add package FransHub.FastProof.NUnit --source /path/to/nupkgs --source https://api.nuget.org/v3/index.json
    # JUnit 5 and TestNG: install the modules into your local Maven repository
    cd reporters/java && mvn -DskipTests install
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

All reporters read the same environment variables as the `fastproof` CLI; without `FASTPROOF_URL` they stay
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
| `FASTPROOF_LOG_LEVEL` | Lowest log level the JVM reporters copy from `java.util.logging` (`INFO` by default) |
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

## Vitest

```sh
npm install --save-dev @franshub/fastproof-vitest @franshub/fastproof-playwright
```

```ts
// vitest.config.ts
import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    retry: 2,
    reporters: ['default', ['@franshub/fastproof-vitest', { environment: 'staging' }]],
    setupFiles: ['@franshub/fastproof-vitest/setup'],
  },
});
```

Vitest 4.1 or newer (the exact attempt list needs Vitest 5) and Node.js 20 or newer. Every retry and repeat is
an attempt with its own status, error, output and trace id. Vitest hands task events to the reporter in
batches, so a test is `running` a moment after it starts. Console output of a test without retries streams
live; for tests with retries it is held until the test ends so that each line lands on the attempt that wrote
it. Files from `annotate()` attachments and browser-mode artifacts are uploaded with the last attempt.

- `fastproofTrace(task)` from `@franshub/fastproof-vitest/trace` returns the `traceparent`, `baggage`,
  `headers({ newSpan })` and `env()` of the running attempt.
- Test case ids and labels come from the test's `meta`: `test('pays', { meta: { fastproofId: 'CASE-7',
  fastproofLabels: { owner: 'team-a' }, fastproofLinks: ['https://tracker.example.com/T-1'] } }, ...)`.
- With a test plan (`FASTPROOF_TESTPLAN`), the `setup` file skips tests the plan does not name and the
  reporter leaves them out of the launch. Selectors by line (`file:12`) need `includeTaskLocation: true`.
- Watch mode is not supported: the reporter stays quiet when Vitest runs with `--watch`.

## Jest

```sh
npm install --save-dev @franshub/fastproof-jest @franshub/fastproof-playwright
```

```js
// jest.config.js
module.exports = {
  reporters: ['default', ['@franshub/fastproof-jest', { environment: 'staging' }]],
  setupFilesAfterEnv: ['@franshub/fastproof-jest/setup'],
};
```

Jest 29 or newer and Node.js 20 or newer. With `jest.retryTimes(n)` Jest reports each attempt, and each becomes
an attempt of the result. The reporter alone sends results, statuses, errors and durations; the `setup` file
(in the test processes) adds what Jest does not report per test: console output, steps and attachments. They
pass through a private temporary directory the reporter creates for the run, so parallel workers need no
credentials and no extra connections.

```js
const { attach, attachFile, step, fastproofTrace } = require('@franshub/fastproof-jest/setup');

test('pays [fastproof-id:CASE-7]', async () => {
  console.log('paying');
  await step('call the payment API', async () => {
    await fetch(url, { headers: fastproofTrace().headers() });
  });
  attach('response.json', JSON.stringify(body), 'application/json');
  attachFile('screenshots/pay.png', 'pay.png', 'image/png');
});
```

- Test case ids are written in the title as `[fastproof-id:ID]` (or `@allure.id=ID`).
- A test file that cannot be loaded is reported as a broken result named `<file> (suite failed to run)`.
- `testPlanPattern()` (from `@franshub/fastproof-jest`) builds a `testNamePattern` from a test plan; the
  reporter leaves out of the launch any test the plan does not select. Selectors by line are not available,
  because Jest does not report test locations.
- Watch mode is not supported.

## Cypress

```sh
npm install --save-dev @franshub/fastproof-cypress @franshub/fastproof-playwright
```

```js
// cypress.config.js
const { defineConfig } = require('cypress');
const { fastproof } = require('@franshub/fastproof-cypress');

module.exports = defineConfig({
  retries: { runMode: 2 },
  e2e: {
    setupNodeEvents(on, config) {
      return fastproof(on, config, { environment: 'staging' });
    },
  },
});
```

```js
// cypress/support/e2e.js
import '@franshub/fastproof-cypress/support';
```

Cypress 16 (the APIs used exist since Cypress 13), `cypress run`. With the support file a test is `running` when it starts, its command log is
sent with the result and every attempt gets a `traceparent` (`fastproofTrace().headers()` in the test). Without
it, results are built from Cypress's own result at the end of each spec: statuses, errors and attempts are the
same, but nothing is live and no `traceparent` reaches the test. Final results are sent when the spec ends,
because the failure screenshot and the video exist only then; screenshots go to the attempt that took them,
and the spec video goes to failed attempts (`video: 'always'` or `'off'` change that).

- Test case ids are written in the title as `[fastproof-id:ID]`.
- With a test plan, the plugin narrows `specPattern` to the files the plan names and skips other tests.
- `cypress open` is not supported.

!!! note "How these three were verified"
    Vitest and Jest reporters were run for real (retries, workers, plans, masking, large attachments) against
    a fake server and a FastProof server built from this repository. The Cypress plugin was driven with the
    event sequence Cypress produces, in the same two ways; the Cypress application itself could not be started
    in the build environment, so a first run in your project is the real check.

## .NET (NUnit, xUnit v3)

```sh
dotnet add package FransHub.FastProof.NUnit    # or FransHub.FastProof.Xunit
```

```csharp
using FransHub.FastProof.NUnit;               // xUnit v3: using FransHub.FastProof.Xunit;
[assembly: FastProofReporter]                 // xUnit v3: add [assembly: CaptureConsole] too
```

```sh
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_PROJECT=shop FASTPROOF_TOKEN=... dotnet test
```

.NET 8 or newer; NUnit 4 (run with 4.6.1 and 5.0.0) or xUnit.net v3 (run with 4.0.2). The reporter stays off
without `FASTPROOF_URL`. It works with `dotnet test` and, for xUnit v3, with the test project's own executable.
`FransHub.FastProof.Core` holds the client, spool, masking and trace context and has no dependency beyond the
base class library.

```csharp
using FransHub.FastProof;

[Test, Retry(3), FastProofId("CASE-7"), Category("checkout")]
public async Task Pays()
{
    Console.WriteLine("paying");
    await FastProofContext.StepAsync("call the payment API", async () =>
        await http.PostAsync(url, content, FastProofContext.Trace.Headers()));
    FastProofContext.Attach("response.json", json, "application/json");
    FastProofContext.AttachFile("screenshots/pay.png");
}
```

- Every run of a test is an attempt: NUnit `[Retry]` and `[Repeat]` passes arrive as attempts of one result with
  their own status, error, output and trace id. `[TestCase]`, `[TestCaseSource]`, `[Theory]` and `[InlineData]`
  rows are separate results with their arguments as parameters. xUnit has no retries of its own; a third-party
  retry attribute that reruns the test is reported the same way.
- Labels: `parentSuite` (namespace), `suite` (class), `subSuite` (nested classes), `thread`, `tag` (NUnit
  `[Category]`, xUnit `Category` trait), and other NUnit `[Property]` values or xUnit traits by name.
  `[FastProofLabel]`, `[FastProofLink]` and `[FastProofId]` add labels, links and an explicit test case id.
- Console output, `ITestOutputHelper` output (xUnit; sampled every 200 ms) and `FastProofContext.Log` stream
  live. `FastProofContext.Trace.Headers()` and `.Env()` carry the attempt's `traceparent` to HTTP calls and child
  processes. Parallel fixtures and collections are safe.
- Test plans (`FASTPROOF_TESTPLAN`): selectors `Namespace.Type.Method` or `Namespace.Type#Method`, with or without
  arguments, or an id (`[FastProofId]`, property or trait `fastproof.id`, `allure_id`, `as_id`). Tests outside
  the plan are skipped and not reported.
- NUnit tests marked `[Ignore]` are reported as skipped; xUnit tests with a static `Skip` never reach the
  framework's extension points and are not reported, while `Assert.Skip` at run time is.
- A failing NUnit `[OneTimeSetUp]` is not reported (its tests never start), and `TestContext.Out` is not
  captured: write to `Console` or `FastProofContext.Log`.
- Separate test projects in one `dotnet test` run are separate processes; set `FASTPROOF_LAUNCH`, or run in CI
  where the launch id comes from the pipeline, so they share one launch.

## JUnit 5 and TestNG

```xml
<dependency>
  <groupId>io.franshub</groupId>
  <artifactId>fastproof-junit5</artifactId>
  <version>0.1.0</version>
  <scope>test</scope>
</dependency>
```

Use `fastproof-testng` for TestNG. Nothing else changes in the project: the jar registers itself through the
`ServiceLoader` of the JUnit Platform (a `TestExecutionListener`, a `LauncherSessionListener` and a
`PostDiscoveryFilter`) or of TestNG (`ITestListener`, `IConfigurationListener`, `IMethodInterceptor`), so
Maven Surefire picks it up (Gradle and the IDEs use the same mechanism, but only Surefire was run). Java 17 or newer (built for 17, tested on 17 and 25); JUnit 5.10 to
5.14 and 6.1 (Platform 1.10 and newer), TestNG 7.11 and 7.12. The jars have no runtime dependencies.

```sh
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_PROJECT=shop FASTPROOF_TOKEN=... mvn test
```

Settings come from the environment (the table above) or from `fastproof.*` system properties
(`-Dfastproof.url=...`), which win. Every test is `running` when it starts and gets its result when it ends.
Standard output and error, `java.util.logging` and `System.Logger` records, and steps reach the launch page
while the test runs, parallel execution included (output belongs to the test whose thread wrote it, or a thread
that test started). The summary line is printed when the test session ends; a JVM shutdown hook ends the run.

```java
import io.franshub.fastproof.FastProof;

FastProof.step("call the payment API", () -> {
    http.post(url, FastProof.trace().orElseThrow().headers(false));
});
FastProof.attach("response.json", "application/json", body);
FastProof.attachFile(Path.of("target/screenshot.png"));
```

- **Retries are attempts.** A Surefire rerun of a failed JUnit test (`rerunFailingTestsCount`), and a TestNG
  `IRetryAnalyzer` retry, are attempts of one result, each with its own status, error, output and trace id. For TestNG, `Retry`
  (`@Test(retryAnalyzer = Retry.class)`) retries up to `fastproof.testng.retries` times (2 by default), and
  setting `-Dfastproof.testng.retries=N` retries every test that has no analyzer of its own. JUnit has no retry
  of its own; extensions that run a test again as new invocations (JUnit Pioneer `@RetryingTest`) show as
  separate results.
- **Separate results.** `@ParameterizedTest`, `@RepeatedTest`, `@TestFactory`, TestNG data provider rows and
  `invocationCount` runs are results of their own. A parameter named `invocation` carries the display name
  (JUnit) or the invocation number (TestNG); with `@ExtendWith(FastProofExtension.class)`, or Jupiter extension
  autodetection (`junit.jupiter.extensions.autodetection.enabled=true`), the argument values are reported as
  parameters instead (with `javac -parameters` under their real names).
- **Names and labels.** `@DisplayName` is the name, `pkg.Class#method(types)` the stable full name. Labels:
  `framework`, `language`, `package`, `testClass`, `suite` (top-level class), `subSuite` (nested classes),
  `testMethod`, `thread`, and for TestNG `parentSuite` and `testng.test`. Tags (JUnit) and groups (TestNG)
  become `tag` labels; `fastproof.id:ID` sets the test case id and `fastproof.label.name:value` adds a label.
- **Skips and setup failures.** `@Disabled`, failed assumptions and `SkipException` are `skipped` with the
  reason; a failing `@BeforeAll` or TestNG configuration method is reported as a broken result, so a class that
  never ran does not disappear. Assertion errors are `failed`, every other exception `broken`.
- **Test plans.** `FASTPROOF_TESTPLAN` (an Allure `testplan.json`) selects tests by `pkg.Class#method`,
  `pkg.Class.method` (with or without parameter types) or by `fastproof.id:` tags and groups; unreadable plans
  are ignored with a warning.
- **Spool.** `java -jar fastproof-core-0.1.0.jar flush [--spool DIR]` sends what an earlier run left;
  `list` shows it. The next run sends it too, and so does any other FastProof reporter's `flush`.

The reporters were run for real (Surefire reruns, TestNG retries, parallel methods, plans, masking, a server
that is down, a server that is stage-B only) against a fake server and a FastProof server built from this
repository.

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

