# ReportPortal agent recordings

Real traffic of the official ReportPortal agents against `recorder/recorder.py`, a minimal
ReportPortal-shaped fake. One NDJSON line per exchange: `seq`, `method`, `path`, `query`,
`headers` (content-type, accept, user-agent only), `authorization` (present or absent, never the
value), `body` (JSON) or `multipart` (parts with size and sha256 for files, parsed JSON for
`json_request_part`), and the recorder's `response`. Faults injected on purpose carry `"injected": true`.

Sanitized by `recorder/sanitize.py`: uuids renumbered in order of first appearance, local paths and
the recorder address replaced by example values. Timestamps are kept as the agents sent them.

| File | Agent | What it covers |
|---|---|---|
| `pytest-plain` | pytest-reportportal 5.6.13 (client 5.7.12), v2 | passes, failures, skip, xfail, parametrize, `issue` marker with external issue, rerunfailures, setup and teardown errors, nested classes, logs at all levels, attachments (text, png, binary) |
| `pytest-xdist-join` | same, `-n 2` with `rp_launch_uuid` | no launch start or finish calls, items into a foreign launch |
| `pytest-fault-503` | same | two `POST item` answered with 503: the agent does not retry |
| `playwright-v2-steps` | agent-js-playwright 5.5.0 (client-javascript 5.6.0), `/rp/api/v2` | 2 spec files, `includeTestSteps`, retries 1 (first attempt fails), screenshots, `trace.zip`, test.step nesting, skip, tag, console logs, attachments |
| `playwright-v1-steps` | same, `/rp/api/v1` | same scenario on v1 |
| `playwright-v2-nosteps` | same, `includeTestSteps: false` | tests only |
| `playwright-v2-shard1-join`, `playwright-v2-shard2-join` | same, `launchId` and `--shard=N/2` | two shards joining one launch |
| `playwright-fault-503` | same | two `POST item` answered with 503: axios retry |
| `testng-v2-async` | agent-java-testng 5.6.8 (client-java 5.4.12), `rp.reporting.async=true` | retry analyzer, `@BeforeClass` that throws, data provider, invocationCount, `SkipException`, dependsOnMethods, logback appender, `ReportPortal.emitLog` with a file, exception logs |
| `testng-sync` | same, `rp.reporting.async=false` | the same on `/api/v1` |
| `testng-join-uuid` | same, `rp.launch.uuid` with `rp.launch.uuid.creation.skip` | no launch calls |
| `robot-deep` | robotframework-reportportal 5.7.3 (client 5.7.12), Robot Framework 7.5.1 | suite setup and teardown, nested keywords, FOR and IF, skip, fail, HTML log |

`recorder/scenarios/` holds the projects that produced them. `recorder/recorder.py --out raw.ndjson
--port 8089 [--fault METHOD:REGEX:STATUS:COUNT]` is the fake server; point the agent at
`http://127.0.0.1:8089/rp` (JS: `http://127.0.0.1:8089/rp/api/v2`), project `example-project`, any api key.
Sanitize with `recorder/sanitize.py RAW OUT --replace FROM=TO`.

Install, pinned: `pip install pytest-reportportal==5.6.13 pytest-rerunfailures pytest-xdist`,
`pip install robotframework robotframework-reportportal==5.7.3`, `npm ci` style install of
`recorder/scenarios/playwright/package.json` (Chromium for Playwright 1.56.1), Maven with
`recorder/scenarios/testng/pom.xml` (JDK 17+).
Run: `pytest --reportportal`, `npx playwright test`, `mvn test`,
`robot --listener robotframework_reportportal.listener --variable RP_ENDPOINT:... suite`.
The Playwright `RP_API_VERSION`, `RP_STEPS` and `RP_LAUNCH_ID` environment variables select the variants.

## Contract tests and snapshots

`crates/fastproof-server/tests/rp_contract.rs` replays every recording against the real `/rp` routes (direct, and Kafka
when `FASTPROOF_TEST_KAFKA_BROKERS` is set) and compares what a user sees (launch counts, results, attempts, steps,
logs per level, attachments, links, labels) with `snapshots/<recording>.json`. Rewrite them with
`FASTPROOF_UPDATE_SNAPSHOTS=1 cargo test -p fastproof-server --test rp_contract`, and read the diff before committing.
Requests the recorder answered with an injected fault are skipped on replay (the agents saw them fail).
`bench/rp_bench.py` is the synthetic agent load of design section 9; `reporters/integration/test_rp_agents.py` runs the
real pytest and Playwright agents against a real server (`FASTPROOF_TEST_RP_AGENTS`).
