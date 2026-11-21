# Reporters

FastProof accepts the report files your tests already write (see [Report formats](formats.md)). Its own
reporters go further: they send results while tests run, stream logs live, keep every retry as an attempt, and
never fail your test run when the server is unreachable.

| Reporter | Package | Status |
|---|---|---|
| Playwright | `@franshub/fastproof-playwright` (npm) | First release in preparation |
| pytest | `pytest-fastproof` (PyPI) | First release in preparation |

More frameworks follow; until then, upload the files of the framework's own reporter with `fastproof upload`.

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
- **Test plans:** an Allure-format `testplan.json` (`FASTPROOF_TESTPLAN`) selects which tests run.

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
| `FASTPROOF_TESTPLAN` | Path of an Allure-format `testplan.json` |
| `FASTPROOF_QUIET` | `true` prints nothing but errors |

Give the token through your CI system's secret store, never in the repository.

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
`-s`). Lower the log level (`log_level = INFO` in the ini file) to stream more than warnings.

Send what earlier runs left in the spool:

```sh
python -m pytest_fastproof flush
```
