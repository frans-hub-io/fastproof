# Live logs

FastProof shows test output while tests run: open a launch, pick a running test and its log grows in the
browser. Results can be opened before they finish (`status: "running"`) and are completed later by the same
client id, so the launch page shows what is running right now. The client id wins: when the final line
carries more identity than the running one (a `testCaseId`, `parameters`, another `fullName`), the attempt
moves to the result of the final identity and an emptied running result is removed (see
[PROTOCOL §3](https://github.com/frans-hub-io/fastproof/blob/main/reporters/PROTOCOL.md)).

The [reporters](reporters.md) stream logs for you. This page describes what they send, for anyone writing
their own integration.

## Sending logs

```http
POST /api/v1/projects/{key}/launches/{id}/logs
Authorization: Bearer <token with launches:write>
Content-Type: application/x-ndjson
```

```json
{"resultId":"checkout-1","attempt":0,"seq":1,"ts":"2026-11-21T10:00:00.120Z","level":"info","stream":"log","message":"opening the cart"}
{"resultId":"checkout-1","attempt":0,"seq":2,"ts":"2026-11-21T10:00:00.480Z","level":"error","stream":"stderr","message":"timeout waiting for #pay"}
```

A JSON array of the same objects is accepted too.

| Field | Meaning |
|---|---|
| `resultId` | The client id of the result, the same one used when the result is reported |
| `attempt` | Attempt number, starting at 0; retries get their own log |
| `seq` | Sequence number within the attempt; lines are shown in this order |
| `ts` | Time of the line: milliseconds since the Unix epoch (`1795341600120`) or an RFC 3339 string (`"2026-11-21T10:00:00.120Z"`); returned as milliseconds |
| `level` | `trace`, `debug`, `info`, `warn` or `error`. `warning` is read as `warn`, `fatal` and `critical` as `error` (stored and shown as `error`); any other value rejects the line |
| `stream` | `stdout`, `stderr`, `log` or `step` |
| `message` | The line |
| `step` | Optional step name (at most 1 KiB) |

The answer is `202` with `{"accepted": 2, "duplicates": 0, "rejected": []}`. Lines are idempotent by
`(resultId, attempt, seq)`: resending a batch after a timeout does not duplicate anything, and lines already
stored are counted as `duplicates`. A line that breaks a rule (for example an unknown level or a `resultId`
over 256 bytes) is listed in `rejected` with its index and the reason; the other lines are stored. A message
over the line limit is not rejected: the server cuts it at a character boundary and ends it with
` …[truncated]`. Mask secrets before sending: the reporters do it at the source.

| Limit | Default | Server setting |
|---|---|---|
| Message of one line | 64 KiB | `FASTPROOF_LOG_MAX_LINE_BYTES` |
| Lines per request | 5000 | `FASTPROOF_LOG_MAX_LINES_PER_REQUEST` |
| Request body | 8 MiB | `FASTPROOF_LOG_MAX_REQUEST_BYTES` |
| Log bytes per launch | 256 MiB | `FASTPROOF_LOG_MAX_BYTES_PER_LAUNCH` |
| `resultId` | 256 bytes | fixed |

A request over the request, line-count or per-launch limit is refused with `413`; the message names the
limit and its unit, for example `the body exceeds max_lines_per_request (max 5000 lines)`. See also
[Limits](../operations/limits.md).

## Following logs

```http
GET /api/v1/projects/{key}/launches/{id}/logs/stream?resultId=&level=&q=
Accept: text/event-stream
```

A server-sent event stream. It starts with a `ready` event, then sends `log` events with
`{"lines": [...]}` (at most 20 events per second per subscriber, up to 500 lines each). Each subscriber
buffers up to 2000 lines; when a client reads too slowly, the oldest lines are dropped and a `gap` event with
`{"skipped": n}` says how many, so fetch them from the stored logs if needed. A stream ends after an hour;
clients reconnect and are authenticated again. Filters: `resultId`, minimum `level`, and a text `q`.

`resultId` is either the client id of one attempt (as sent by the reporter) or the id of a result in the
launch: then the lines of all its attempts are returned. Each attempt in `GET .../results/{id}` carries its
`clientId` when it was reported live.

With several replicas every replica sees every line: through PostgreSQL `LISTEN/NOTIFY` in direct mode, or by
reading the `ingest.logs` Kafka topic in Kafka mode.

## Reading stored logs

```http
GET /api/v1/projects/{key}/launches/{id}/logs?resultId=&attempt=&level=&q=&before=&limit=
```

Returns `{"items": [...], "next": "..."}`: the newest matching lines before the cursor, oldest first. Pass
`next` as `before` to page to older lines. `limit` is 1 to 5000 (default 500), `q` is a case-insensitive text
of at most 256 bytes. Logs are stored as compressed chunks per attempt in PostgreSQL, and copied to ClickHouse
or OpenSearch when a sink is enabled.

## Searching logs

```http
GET /api/v1/projects/{key}/search/logs?q=&launchId=&from=&to=&level=&limit=
GET /api/v1/projects/{key}/results/{id}/similar
```

Search across launches and "similar failures" run on the configured search backend: PostgreSQL by default,
ClickHouse or OpenSearch for large volumes (see [Scaling and big data](../operations/scaling.md)). `q` has 1 to
256 bytes (a substring in PostgreSQL and ClickHouse, a phrase in OpenSearch); `from` and `to` are RFC 3339 and
default to the last 7 days; `limit` is 1 to 1000 (default 100).

All endpoints need a token or session with read access to the project; sending needs `launches:write`.
