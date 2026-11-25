# Migrating from ReportPortal

FastProof accepts the reporting part of the ReportPortal API, so the official ReportPortal agents (Java JUnit 5
and TestNG, pytest, Robot Framework, Playwright, Jest, Cypress and the others built on the ReportPortal
clients) report into FastProof without changes to the tests. Only three settings of the agent change: the
endpoint, the project and the API key.

The agent calls are translated into the native model: test items become results, retries become attempts of
one result, nested steps become steps, log lines become [live logs](live-logs.md), and files become
attachments. A `trace.zip` of the Playwright agent opens in the trace viewer like one from the native reporter.

This is a migration path. The native [reporters](reporters.md) do more: `traceparent` per attempt, an offline
spool when the server is down, masking of secrets at the source and test selection by `testplan.json`.

## Point the agent at FastProof

| Setting | Value |
|---|---|
| Endpoint | `https://fastproof.example.com/rp` (the JavaScript client takes `https://fastproof.example.com/rp/api/v2`) |
| Project | The key of a FastProof project, or a ReportPortal project name listed in its `rp.aliases` setting |
| API key | A FastProof token (`fp_…`) with the `uploader` role on the project |

=== "Java (JUnit 5, TestNG)"

    `reportportal.properties`:

    ```properties
    rp.endpoint = https://fastproof.example.com/rp
    rp.project = shop
    rp.api.key = fp_xxxxxxxx
    rp.launch = nightly
    ```

=== "pytest"

    `pytest.ini`:

    ```ini
    [pytest]
    rp_endpoint = https://fastproof.example.com/rp
    rp_project = shop
    rp_api_key = fp_xxxxxxxx
    rp_launch = nightly
    ```

=== "Playwright"

    `playwright.config.ts`:

    ```ts
    const rpConfig = {
      endpoint: 'https://fastproof.example.com/rp/api/v2',
      project: 'shop',
      apiKey: process.env.FASTPROOF_TOKEN,
      launch: 'nightly',
      includeTestSteps: true,
    };
    export default defineConfig({ reporter: [['@reportportal/agent-js-playwright', rpConfig]] });
    ```

=== "Robot Framework"

    ```sh
    robot --listener robotframework_reportportal.listener \
      --variable RP_ENDPOINT:https://fastproof.example.com/rp \
      --variable RP_PROJECT:shop --variable RP_API_KEY:fp_xxxxxxxx \
      --variable RP_LAUNCH:nightly tests/
    ```

Both `/rp/api/v1` and `/rp/api/v2` work and behave the same. `GET /rp/api/info` reports version 5.14.0, so
agents send ISO-8601 times; epoch milliseconds are accepted too.

## Projects

A path segment `{project}` is first compared with the project keys, then (without case) with the
`rp.aliases` of the projects. Projects are not created on the fly and names are not normalized: to keep
`default_personal` or `Team_A` in the agent configuration, list it in the project settings:

```json
{"rp": {"aliases": ["Team_A", "default_personal"]}}
```

An alias belongs to one project only. An unknown project and a project the token may not write to get the same
answer as in the native API.

## Shards and parallel jobs: one launch per pipeline

Agents can report into a launch they did not start. Start the launch once with the CLI, pass its id to every
job as the ReportPortal launch uuid, and finish it at the end:

```sh
export FASTPROOF_URL=https://fastproof.example.com FASTPROOF_TOKEN=fp_xxxxxxxx FASTPROOF_PROJECT=shop
FP_LAUNCH=$(fastproof launch start)        # CI and VCS metadata come from the CI variables
# in each job or shard:
#   Java:       -Drp.launch.uuid=$FP_LAUNCH -Drp.launch.uuid.creation.skip=true
#   pytest:     -o rp_launch_uuid=$FP_LAUNCH
#   Playwright: launchId: process.env.FP_LAUNCH
#   Robot:      --variable RP_LAUNCH_UUID:$FP_LAUNCH
fastproof launch finish                    # after the last job
```

This is the recommended setup in CI: branch, commit and pipeline come from `fastproof launch start`, and every
shard lands in one launch. Without it, each agent run that starts a launch makes its own.

## How items become results

| ReportPortal | FastProof |
|---|---|
| Launch `name`, `description`, `attributes` | Launch name; the description becomes the `description` attribute; `system` attributes (os, jvm, agent) become environment values |
| Attributes `branch`, `commit`/`revision`, `ci.url`, `build` | `vcs.branch`, `vcs.commit`, `ci.url`, `ci.buildOrder` (change the list with `rp.attributeMap`) |
| The attribute named in `rp.environmentAttribute` (off by default) | The environment key of the launch, part of every test's identity |
| `mode: DEBUG` | A debug launch |
| Item with `hasStats` and no tests under it (`STEP`, `SCENARIO`, `TEST`) | A result: one attempt of a test |
| Item with tests under it (`SUITE`, `STORY`, a `TEST` of TestNG) | A container: its name is part of the suite path (`parentSuite`, `suite`, `subSuite`) |
| Item with `hasStats: false` (nested steps, keywords) | A step of the nearest result |
| `BEFORE_*`/`AFTER_*` that failed | A broken result named `<container>: <fixture>` with label `kind: fixture`; passed fixtures are not results |
| `codeRef`, `testCaseId`, `parameters` | Full name, test case id and parameters of the test identity |
| Item attributes | Labels (`key: value`; a value alone is a `tag`) |
| Retries (`retry`, `retryOf`, pytest-rerunfailures, Playwright retries) | Attempts of one result: a failed and a passed attempt make it flaky in the run |
| `rerun: true` | Opens the caller's earlier launch with that name again; new items become new attempts |
| `STOPPED`, `INTERRUPTED`, `CANCELLED` | Broken with message `interrupted` |
| ERROR log lines of a test and its steps | The error message (first line of the last one) and trace of the result |
| Files of log lines | Attachments of the result or step that wrote the line |
| `issue` on finish (`pb001`, `ab001`, … from `GET settings`) | Triage with that defect type when the token has `results:triage`; otherwise a label `rp.issue` with the defect group. External issues become issue links |

Lines of a launch or a container (not of a test) are kept as logs of the launch under the result ids
`rp-launch` and `rp-container:<uuid>`.

## Timing

A finished test is written as a result once it has been quiet for `FASTPROOF_RP_SETTLE_MS` (2 s): no new lines
or files for it. Agents on the ReportPortal Python client (pytest, Robot Framework, behave) send log lines in
batches, often long after the test finished; for their launches (and for any launch where a line arrives after
its result was written) the results are written when the launch finishes, so that error texts and files land
in them. Their tests show as running until then.

The error message of a failed test comes from its ERROR log lines, and some agents (the Java ones in particular)
send their last batch of lines only after the launch finish. So a failed test without an error message waits
for one up to `FASTPROOF_RP_ERROR_WAIT_MS` (30 s) after its finish, and up to the same time after the launch
finish; it shows as running meanwhile, and it is written without a message if none comes. Passed and skipped
tests do not wait. Log lines that come after a result was written are kept in the logs; neither they nor files
that come that late change the result.

When the agent finishes the launch, tests that are still open get `FASTPROOF_RP_FINISH_GRACE_SECONDS` (120 s)
to finish; then they become broken `interrupted` and the launch finishes. A launch without any call for
`FASTPROOF_RP_LAUNCH_IDLE_TIMEOUT` (1 h), for example after the agent crashed, finishes as interrupted.

## Import the history of an existing ReportPortal

`fastproof rp import` copies launches, tests, retries, defect triage, logs and attachments from a running
ReportPortal. It reads the ReportPortal REST API (never its database) and replays every launch through the
`/rp` endpoint of FastProof with the original uuids and times. Imported history and the reports of RP agents
therefore form the same series, and the first new run continues the imported history of each test.

```bash
export RP_API_KEY=...                       # or --rp-key-file; never pass the key as an argument
export FASTPROOF_TOKEN=fp_...               # launches:write; add results:triage to keep the triage

fastproof rp import --dry-run --rp-url https://rp.example.com --rp-project team   # counts only
fastproof rp import --url https://fastproof.example.com --project shop \
  --rp-url https://rp.example.com --rp-project team
```

| Option | Default | Meaning |
|---|---|---|
| `--since` | `90d` | Launches that started within this time (`12h`, `4w`), since a date (`2026-01-31`), or `all`. |
| `--launch-name` | all | Only launches with this exact name. |
| `--include-debug` | off | Also copy DEBUG launches (they stay debug launches). |
| `--logs` | `warn` | `none`, `error`, `warn` (that level and above) or `all`. |
| `--attachments` | `failed` | `none`, `failed` (files of failed tests and their steps) or `all`. |
| `--max-attachment` | `64MiB` | Larger files are skipped and counted. |
| `--dry-run` | off | Reads everything, counts launches, items, log bytes and files, writes nothing. |
| `--state` | `.fastproof/rp-import.json` | The state file. |
| `--rate` | `10` | ReportPortal requests per second at most (ReportPortal has no rate limit of its own). |
| `--parallel` | `2` | Launches replayed at once (1 to 4). |
| `--rp-ca-file` | none | PEM file with the certificate authority of ReportPortal. |

How it behaves:

- **Safe to repeat.** A launch is recorded in the state file when it is replayed completely. An interrupted
  import continues with the launches left; an import with a lost state file replays them again under the same
  uuids, which changes nothing in tests and attempts (log lines may repeat). The state file belongs to one
  source and target and is refused for another.
- **Only finished launches.** A launch still `IN_PROGRESS` in ReportPortal is skipped and counted.
- **Defects.** `issue` is applied as triage when the token has `results:triage`; without it the result gets the
  `rp.issue` label and the ticket links, as for live reports. A custom defect type of ReportPortal becomes
  the built-in type of its group, and its name leads the comment.
- **Retries.** Earlier attempts listed in `retries` are replayed as attempts of the same test.
- **Connection.** HTTPS is required (plain http only to localhost or with `--rp-allow-http`), redirects are not
  followed so the key goes only to the URL you gave, requests are retried with a growing pause on 429 and 5xx,
  and the API key never appears in the state file or the output.
- **Bounds.** A launch with more than 1 000 000 items (earlier attempts included) or more than 512 MiB of item
  data is reported as failed and not imported; so is one whose logs exceed 256 MiB for 50 items in a row (import
  it with `--logs warn` or `error`). A log line longer than 128 KiB is cut. Text that comes from ReportPortal is
  printed with control characters escaped.
- **Not copied.** Dashboards, widgets, filters, users, ML indexes, action history, Quality Gates, notification
  and BTS settings, the ReportPortal test case management, launch-level logs, nested steps of retried
  attempts, and data that the ReportPortal retention removed.

## Not supported

The endpoint covers reporting only. The rest answers `501` with
`{"errorCode": 5000, "message": "not supported by FastProof"}`:

- the ReportPortal UI API: dashboards, widgets, filters, item history and statistics;
- the analyzer, pattern analysis and auto-analysis, Quality Gates, BTS integrations, users and organizations,
  the ReportPortal test case management;
- `launch/merge` (use [merging launches](merging-launches.md)), `launch/import`, `launch/{id}/analyze`;
- UUID API keys of ReportPortal, OAuth password grant and session cookies: use FastProof tokens.

Agents do not mask secrets in log lines. Where logs may hold secrets, prefer the native reporters, which mask
them before they leave the CI job.

## Limits

See [Limits](../operations/limits.md#reportportal-endpoint) and the `FASTPROOF_RP_*` settings in
[Configuration](../operations/configuration.md#reportportal-endpoint).
