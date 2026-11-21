# Live logs

FastProof shows test output while tests run: open a launch, pick a running test and its log grows in the
browser. Results can be opened before they finish (`status: "running"`) and are completed later by the same
client id, so the launch page shows what is running right now.

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
| `ts` | Time of the line (RFC 3339) |
| `level` | `trace`, `debug`, `info`, `warn` or `error` |
| `stream` | `stdout`, `stderr`, `log` or `step` |
| `message` | The line |
| `step` | Optional step name for `stream: step` |

Lines are idempotent by `(resultId, attempt, seq)`: resending a batch after a timeout does not duplicate
anything. Line size, lines per request and bytes per launch are limited; a request over
the limits is rejected with a clear error. Mask secrets before sending: the reporters do it at the
source.

## Following logs

```http
GET /api/v1/projects/{key}/launches/{id}/logs/stream?resultId=&level=&q=
Accept: text/event-stream
```

A server-sent event stream of `log` events, batched to at most 20 per second per subscriber. When a client
reads too slowly, the oldest lines are dropped and a `gap` event says how many; fetch them from the stored
logs if needed. Filters: `resultId`, minimum `level`, and a text `q`.

With several replicas every replica sees every line: through PostgreSQL `LISTEN/NOTIFY` in direct mode, or by
reading the `ingest.logs` Kafka topic in Kafka mode.

## Reading stored logs

```http
GET /api/v1/projects/{key}/launches/{id}/logs?resultId=&attempt=&level=&q=&before=&limit=
```

Pages backwards from `before`. Logs are stored as compressed chunks per attempt in PostgreSQL, and copied to
ClickHouse or OpenSearch when a sink is enabled.

## Searching logs

```http
GET /api/v1/projects/{key}/search/logs?q=&launchId=&from=&to=&level=&limit=
GET /api/v1/projects/{key}/results/{id}/similar
```

Search across launches and "similar failures" run on the configured search backend: PostgreSQL by default,
ClickHouse or OpenSearch for large volumes (see [Scaling and big data](../operations/scaling.md)).

All endpoints need a token or session with read access to the project; sending needs `launches:write`.
