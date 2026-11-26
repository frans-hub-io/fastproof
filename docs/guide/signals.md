# Signals

A test that failed because the browser grid lost its node, because the CI runner ran out of
memory or because Kubernetes evicted the pod is not a product bug. **Signals** let the systems
around your tests tell FastProof what happened to an attempt, and FastProof Analysis reads the
FastMock journal of failed attempts for the same reason: to tell infrastructure and test-data
problems from real failures.

Signals **never change a defect**. They are stored next to the attempt and only add weight:

- to [triage suggestions](analysis.md): an exact grid, CI or Kubernetes signal adds 0.30 (0.15
  by time window) to the best `System issue` candidate of the cluster; FastMock `unmatched` and
  `stub_error` add to `Automation bug`, `injected_fault` to `No defect`. Suggestions stay a
  transparent nearest-decision search over what people decided before, not a self-learning
  model: a signal only raises a candidate that people's decisions already gave;
- to [flaky-cause hints](flaky-causes.md): failures with infrastructure signals count towards
  the `infra` cause and the infrastructure share of a flaky test;
- to [rules](rules-and-triage.md#log-patterns): `match.signals` matches results with a stored
  signal of a source (and kind).

Signals need FastProof Analysis on the instance (`FASTPROOF_ANALYSIS=on`) and the project
(Settings → Analysis). The result page lists a result's signals under **Triage → Signals**,
by attempt.

## Sources and kinds

| `source` | `kind` | Sent by |
|---|---|---|
| `grid` | `session_crashed`, `browser_oom`, `startup_timeout`, `node_lost`, `evicted` | A browser grid (any: it pushes, FastProof never polls it) |
| `ci` | `runner_oom`, `pod_evicted`, `job_cancelled` | A CI job or runner hook |
| `k8s` | `pod_evicted`, `oom_killed`, `node_not_ready` | A Kubernetes event exporter |
| `mock` | `unmatched`, `stub_error`, `slow`, `unexpected_calls`, `injected_fault` | FastProof itself, from the FastMock journal (cannot be pushed) |

Other kinds are refused per item (`unknown_kind`).

## Pushing signals

`POST /api/v1/projects/{key}/signals` with a token that has `launches:write` in the project
(a CI token works). Each item names its attempt in one of two ways:

- `traceId`: the 32-hex trace id of the attempt's W3C `traceparent` (the reporters send one per
  attempt, see [Formats](formats.md)); add `launchId` when you know it. A grid can answer with
  the same id when the test passes the attempt's `traceparent` to it, for example as a header
  of the session request (the Playwright and pytest reporters expose the attempt's trace
  headers for this).
- `launchId` + `resultClientId`: the launch and the client id the reporter gave the attempt.

A grid that lost a node during a browser session:

```sh
curl -sS -X POST https://fastproof.example.com/api/v1/projects/web-shop/signals \
  -H "Authorization: Bearer $FASTPROOF_TOKEN" -H 'content-type: application/json' -d '{
  "items": [
    {"traceId": "4bf92f3577b34da6a3ce929d0e0e4736", "source": "grid", "kind": "node_lost",
     "at": "2026-11-25T10:14:03Z",
     "detail": {"node": "grid-node-3", "browser": "chromium", "sessionId": "b7ad6b7169203331"}}
  ]
}'
```

A CI job whose runner was killed for memory, from an `after_script` or a failure hook, naming
the attempt by its client id:

```sh
curl -sS -X POST https://fastproof.example.com/api/v1/projects/web-shop/signals \
  -H "Authorization: Bearer $FASTPROOF_TOKEN" -H 'content-type: application/json' -d '{
  "items": [
    {"launchId": "0199f3c2-7d1e-7a40-9c55-2f4e0b1a6d10", "resultClientId": "py-3f9a1c2b7d4e-1b2c3d4e5f607182-0",
     "source": "ci", "kind": "runner_oom", "at": "2026-11-25T10:20:41Z",
     "detail": {"runner": "runner-12", "limitMb": 4096}}
  ]
}'
```

The answer counts what was stored and lists refused items by index:

```json
{"accepted": 1, "matched": 1, "pending": 0, "rejected": []}
```

- `matched`: the attempt was found at once. `pending`: it was not there yet (a grid often
  reports before the reporter sends the final result); FastProof matches it when the attempt
  arrives (the resolution job runs every 30 seconds, and the launch's analysis matches its own
  signals before it runs). A signal that matches nothing within **two days** is deleted.
- A trace id is looked for in launches created from a day before the signal's `at` to an hour
  after it (or only in `launchId` when given).
- Pushing the same signal again (same source, kind, `at` and addressing) is refused as
  `duplicate`, so retries are safe.

### Limits

| Limit | Value | Answer |
|---|---|---|
| Items per request | 1 to 1000 | `400 bad_request` |
| Request body | 1 MiB | `413 payload_too_large` |
| `at` | within the last 7 days and at most 5 minutes ahead of the server clock | item `too_old` |
| `detail` | a flat object of strings, numbers and booleans; keys of 1 to 64 bytes | `400 bad_request` |
| `detail` serialized | 4 KiB | item `detail_too_large` |
| Addressing | `traceId` (32 hex, not all zeros) or `launchId` + `resultClientId` (1 to 256 bytes), not both; the launch must belong to the project | item `bad_address` |
| Signals per attempt | 50 | item `attempt_full` |
| Signals per project | `FASTPROOF_ANALYSIS_SIGNALS_PER_MINUTE` a minute on each replica (600 by default); one request of up to 1000 items passes when the budget is full | `429 rate_limited` with `Retry-After` |

With Analysis off for the instance or the project the push answers `409 analysis_disabled`.
Pushes are not written to the audit log; they are counted in
`fastproof_analysis_signals_total{source, outcome}`.

### What to put in `detail`

`detail` is shown on the result page as plain text to everyone who can read the project's
launches (`launches:read`), and it stays as long as the launch. Put identifiers that help
someone find the cause (node name, browser, session id, runner, memory limit), never secrets,
tokens, URLs with credentials or personal data. FastProof does not mask `detail`; it turns line
breaks and other control characters into spaces and drops invisible characters (bidirectional
controls, zero-width characters), so a value cannot fake another line or hide text.

## FastMock signals

When a project has [FastMock settings](concepts.md#fastmock-correlation) (`fastmock.url` and,
for a protected admin API, `fastmock.tokenSecretRef`), the analysis of a finished launch reads
the FastMock journal of its failed attempts (newest first, at most
`FASTPROOF_ANALYSIS_FASTMOCK_ATTEMPTS`, 100 by default, `0` turns it off) through the same
correlation as the result page's **Mocks** tab: by the attempt's trace id when FastMock
recorded it, otherwise by its time window (and FastMock space). It uses the same egress
allowlist (`FASTPROOF_EGRESS_ALLOW`), reads at most 4 attempts at once and stops after 30
seconds per launch, or once it holds 20 000 journal entries for the launch (parallel attempts
share time windows and so the same entries); the step is then `partial`.

For each attempt it writes one signal per kind:

| `kind` | When |
|---|---|
| `unmatched` | A request matched no stub, on a route a stub answered elsewhere in the launch: a near miss (wrong body, header or query), usually test data or the stub |
| `unexpected_calls` | A request matched no stub, on a route no stub answered in the launch: the application called something the mocks do not know |
| `stub_error` | A stub answered 4xx or 5xx |
| `slow` | A stub took more than 2 seconds, or more than its own 95th percentile in the launch (with at least 20 calls of that stub) |
| `injected_fault` | FastMock reports a fault it injected on purpose (read when its journal records faults) |

`confidence` is `exact` when the requests were found by the trace id and `window` when only
the time window matched (another test running at the same time may have sent them). `detail`
holds the count, the method and path of the first request (never its query string), its
status and duration, the stub and the FastMock space. The path is kept as the application sent
it and is shown to everyone who can read the project's launches, like the **Mocks** tab: keep
tokens out of request paths. Analysing the launch again refreshes these signals instead of
adding more.

The launch's analysis status (`GET …/launches/{id}/analysis`, step `signals`) shows how many
attempts were read; when FastMock cannot be reached it names the HTTP status, and the server
log has the cause.

## Reading signals

- `GET /api/v1/projects/{key}/results/{id}/signals` (`launches:read`): the result's signals by
  attempt and time, at most 500.
- The launch's suggestions list the signals of each cluster's results, counted by source,
  kind and confidence.

## Settings

| Variable | Default | Effect |
|---|---|---|
| `FASTPROOF_ANALYSIS_SIGNALS_PER_MINUTE` | `600` | Pushed signals per project and replica each minute |
| `FASTPROOF_ANALYSIS_FASTMOCK_ATTEMPTS` | `100` | Failed attempts per launch read from FastMock (0: off) |

See [Configuration](../operations/configuration.md) for the other Analysis settings.
