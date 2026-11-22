# Scaling and big data

FastProof starts as one process with PostgreSQL and object storage, and that is the right setup for most
teams. This page explains the optional parts for large installations: Kafka between the HTTP API and the
database, separate roles, and ClickHouse or OpenSearch for search over large volumes of logs.

## Start simple

The default (`ingest.mode: direct`, one Deployment, `search.backend: postgres`) handles a busy CI farm:

- uploads are spooled, queued in PostgreSQL and parsed by ingest workers on any replica;
- live logs reach the UI through PostgreSQL `LISTEN/NOTIFY` (or Valkey with several replicas);
- log search and similar failures run in PostgreSQL over compressed log chunks.

Add replicas (with S3 or a ReadWriteMany volume) before adding anything else. Move on only when one of the
signals below appears.

## When to enable what

| Signal | What helps |
|---|---|
| Upload bursts make the API slow, or the ingest queue grows during peak hours | `roles.split: true`, more `roles.ingest` replicas |
| Many agents stream live logs at once and PostgreSQL write load dominates | `ingest.mode: kafka` |
| PostgreSQL storage grows mostly from logs, or log search gets slow | A sink (ClickHouse or OpenSearch) and `search.backend` |
| Long-term trends over many months and projects | ClickHouse sink (daily aggregates are kept after raw rows expire) |
| An OpenSearch cluster already runs logs for your platform | OpenSearch sink |

## Direct or Kafka

| | `direct` | `kafka` |
|---|---|---|
| Path | HTTP, spool, PostgreSQL job queue, ingest workers | HTTP gateway, Kafka topics, writer consumers |
| Extra moving parts | none | Kafka |
| Back pressure | PostgreSQL queue depth | Kafka retention absorbs bursts; writers catch up |
| Live logs to the UI | PostgreSQL `LISTEN/NOTIFY` | Each API replica reads `ingest.logs` from the latest offset |
| Ordering | per upload | per launch (records are keyed by launch id) |

In Kafka mode only the gateway produces, after token authentication, unless
`FASTPROOF_KAFKA_TRUSTED_PRODUCERS=true`. Consumer groups are coordinated by FastProof in PostgreSQL (Kafka
consumer groups are not used), and the writer commits its offset in the same transaction as the data, so
retries never duplicate results. FastProof exports consumer lag on `/metrics`.

Launches that run the same tests are written at the same time by several writers (or ingest workers) without
waiting on each other for long: the rows they share (test cases, series, statistics) are written in key order
and left alone when nothing changed, and a transaction that still meets a deadlock or a serialization failure
runs again after a short pause (counted in `fastproof_db_tx_retries_total`).

Topics (after `FASTPROOF_KAFKA_TOPIC_PREFIX`, default `fastproof.`): `ingest.results`, `ingest.logs`,
`ingest.uploads` and `events`. Create them yourself with the partition count you want, or let FastProof create
them (`kafka.topics.create`, always on for the bundled Kafka of `kafka.enabled`, whose broker does not create
topics by itself).

## Roles

| Role | Does |
|---|---|
| `api` | HTTP API, UI, live event streams; in Kafka mode the gateway that produces |
| `ingest` | Parses uploaded archives (allure-results, JUnit, CTRF, Playwright) and stores attachments |
| `writer` | Kafka mode: consumes results and logs and writes them to PostgreSQL |
| `sink` | Copies results, attempts, steps and logs to ClickHouse or OpenSearch |
| `scheduler` | Periodic jobs (partitions, retention); safe on any number of replicas |

`FASTPROOF_ROLES=all` (the default) runs everything in one process. With `roles.split: true` the Helm chart
runs `api` (with `scheduler`), `ingest`, `writer` and `sink` as separate Deployments, each with its own
replicas, resources, probes, HPA and PDB. See [Helm chart](helm.md#separate-roles).

## ClickHouse or OpenSearch

| | ClickHouse | OpenSearch |
|---|---|---|
| Best at | Aggregates and long-term trends, compact storage of large log volumes | Relevance-ranked full-text search, existing OpenSearch platforms |
| FastProof stores | Tables `results`, `attempts`, `steps`, `logs` (MergeTree, partitioned by month, TTL) and `series_daily` aggregates | Data streams such as `<prefix>logs-*`, expired by an ISM policy |
| Log search | Text index on messages (ClickHouse 26.2 or newer; older servers use bloom-filter indexes) | Full-text queries |
| Similar failures | Same fingerprint, then n-gram distance of normalized messages | Same fingerprint, then full-text match of normalized messages |
| Memory for a small node | About 2 GiB | About 1.5 GiB (768 MiB heap) |
| Supported versions | 26.8 LTS recommended; 26.2 or newer for the text index | 2.19 and 3.x |

Both sinks are fed from FastProof's event outbox, never from the API path, so a slow or unavailable sink does
not slow uploads; it catches up later. PostgreSQL stays the source of truth. Both can run at once
(`FASTPROOF_SINKS=clickhouse,opensearch`); `search.backend` picks which one answers search.

### ClickHouse for serious use

The chart's bundled ClickHouse is one node without replication, sized for a lab or a small team. For
production, run ClickHouse with the [Altinity ClickHouse operator](https://github.com/Altinity/clickhouse-operator)
(chart `altinity-clickhouse-operator` 0.27.x) and point FastProof at it with `sinks.clickhouse.external.*`:

- a `ClickHouseInstallation` with at least two replicas per shard and a `ClickHouseKeeperInstallation`
  (three Keeper nodes) for replication;
- set the pod and container security context in the CHI `podTemplate` yourself (the operator's defaults are
  empty): non-root UID 101, `RuntimeDefault` seccomp, all capabilities dropped;
- expose only the HTTPS interface (8443) to FastProof; keep the native port 9000 closed with a NetworkPolicy;
- create a `fastproof` user with `GRANT ALL ON fastproof.*` and a profile with `max_memory_usage`;
- pin the image by digest and review patch notes for "native protocol", "privilege" and "crash" entries.

FastProof creates its tables on start. On a replicated cluster, create the database with the `Replicated`
engine so DDL reaches every replica.

### OpenSearch for serious use

Use a managed OpenSearch or the official chart with three or more nodes, real certificates (cert-manager) and
the security plugin on. Give FastProof a user whose role has `indices_all` on `fastproof-*` and
`.ds-fastproof-*`, index template and ISM policy management, and `cluster_monitor`. Never disable the security
plugin.

## Sizing

Start from these and measure with `fastproof bench`:

| Setup | FastProof | PostgreSQL | Kafka | ClickHouse | OpenSearch |
|---|---|---|---|---|---|
| Small team, direct | 1 replica, 1 CPU, 1 GiB | 1 CPU, 1 GiB | none | none | none |
| CI farm, direct | 2-4 replicas, 2 CPU, 1 GiB each | 2-4 CPU, 4-8 GiB | none | none | none |
| Lab big data (`ci/bigdata-values.yaml`) | api, ingest, writer, sink at 0.5-1 CPU, 384-768 MiB | 1 CPU, 768 MiB | 1 broker, 768 MiB (384 MiB heap) | 1 node, 2 GiB | none |
| Large, Kafka | api 2+, ingest 2+, writer 2+, sink 1-2 | HA cluster | 3 brokers over TLS (managed, or `kafka.replicaCount: 3`) | 2+ replicas | 3+ nodes |

- Kafka partitions bound writer parallelism: run at most as many writer replicas as partitions of
  `ingest.results`.
- Size the PostgreSQL connections for all Deployments: replicas × `database.maxConnections` summed over roles.
- Run one heavy backend at a time on small nodes: ClickHouse, OpenSearch and Kafka together need about
  4.5 GiB.

## Memory of the Kafka consumers

Every process that consumes Kafka (the writer, the upload consumer of the `ingest` role, the sinks and the live
log readers of the `api` role) keeps its memory within settings, whatever the lag, the number of partitions it
holds or the size of the records:

- **One budget per process.** Before a partition fetch is sent it reserves `FASTPROOF_KAFKA_FETCH_MAX_BYTES`
  from `FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES`; when the budget is spent, fetches wait (back pressure) until
  earlier records are committed. The records a process holds stay within the budget plus at most one record
  batch per fetch.
- **Decoded size, not wire size.** Kafka counts fetch bytes compressed. FastProof starts each partition with
  a small request and adapts it to how much the records expand, so a fetch decodes to about
  `FASTPROOF_KAFKA_FETCH_MAX_BYTES`.
- **Bounded transactions.** Fetched records are handled and committed in batches of at most
  `FASTPROOF_KAFKA_BATCH_MAX_RECORDS` records and `FASTPROOF_KAFKA_BATCH_MAX_BYTES` bytes; raw records are
  dropped as they are decoded, and each committed batch returns its bytes to the budget.
- **Uploads** are parsed from a file streamed out of blob storage, never held whole in memory, and at most
  `FASTPROOF_INGEST_WORKERS` archives are parsed at once per process, however many partitions have uploads
  waiting.
- **Sinks** load and write at most 1000 results, 250 attempts or about 4 MiB of log lines at a time, with at
  most two writes at once per sink and process.
- **Live log readers** fetch only while someone follows a launch on that replica, and decode only the records
  of followed launches.

A writer needs about 30 MiB at rest and stayed under 140 MiB while draining a large backlog, at either budget
below; decoded rows, database statements and allocator overhead come on top of the raw records the budget
counts. The chart's defaults (64 MiB budget, 512 MiB memory limit for `writer` and `sink`) leave room; lower
the budget before lowering the limit. `fastproof_kafka_inflight_bytes` shows the budget in use and
`fastproof_kafka_budget_waits_total` counts fetches that had to wait.

| Writer draining 268 MiB of backlog (10,000 results, 200,000 log lines, 20 launches sharing tests) | Peak memory | Drain time |
|---|---|---|
| Before (fixed 8 MiB fetches on every partition, one transaction per fetch) | 530 MiB | 25 to 34 s |
| `FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES` 64 MiB (default) | 125 to 135 MiB | 3 to 4 s |
| `FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES` 32 MiB | 126 MiB | 4 s |

Measured with the static release build, one writer holding 12 partitions, on a shared development VM.

## Measured numbers

The numbers below come from one development VM that also ran other workloads, with PostgreSQL 18 and the
default settings of FastProof. Treat them as an order of magnitude, not a promise; measure your own setup with
`fastproof bench`.

### Log search in PostgreSQL

| Measurement | Result |
|---|---|
| Log lines loaded and indexed | 5.5 million (about 1.3 GiB of table, 0.5 GiB of message indexes) |
| Full-text search (words, words plus a level, a rare token) | under 20 ms, extrapolated to under 20 ms at 10 million lines |
| Selective substring search (trigram index, rare text or an id fragment) | under 100 ms |
| Unselective substring search (common words, first 50 hits) | 120 to 250 ms |

PostgreSQL search is fine up to around ten million log lines. Move log search to ClickHouse
(`search.backend: clickhouse` with the ClickHouse sink) from tens of millions of lines, or earlier when people
often search for unselective substrings (common words, partial stack frames) across many launches.

### Ingest throughput

| Path | Results per second | Log lines per second | Lag |
|---|---|---|---|
| `direct` (HTTP, spool, PostgreSQL queue) | 1,516 | 30,000 | none beyond the request |
| `kafka`, produced through the HTTP gateway | 492 | not measured separately | about 32 s from request to PostgreSQL |

Kafka mode is not tuned yet. Today it pays off for absorbing bursts and decoupling the API from PostgreSQL,
not for raw throughput; use `direct` unless one of the signals above applies.

## Benchmark

`fastproof bench` generates launches, results and log lines with realistic failure patterns, pushes them
through the HTTP API (or straight to Kafka), and reports throughput and server-side lag. Run it as a
Kubernetes Job next to the installation you want to measure.

### Test management

`fastproof bench tm` loads test management through the public API only, with the same `FASTPROOF_URL` and
token as `fastproof bench` (an admin token: it creates and deletes projects). It creates a project with a
random key (or `--project`), then:

1. 30 custom fields of mixed types and a folder tree of 300 folders up to 5 levels deep;
2. 20,000 cases with 12 steps on average (some nested), random fields and tags, 4 more versions of 2,000 of
   them (`PUT` with `If-Match`) and 200 saves of a case with 100 steps;
3. an allure-results upload of 5,000 tests on `main`, 2,000 of them with `ALLURE_ID` labels naming existing
   cases, and waits until code sync has linked them;
4. 200 samples each of a list page (50) with two filters, a tree level with counts and a case;
5. a managed plan of 5,000 cases (2,000 manual cases by a tag filter, the rest automated by number) and a
   launch started from it;
6. 50 virtual testers executing the 2,000 manual run items (25 steps each, 50,000 step results), each step
   written separately, then a final status;
7. the automated part uploaded into the plan launch, and the manual run closed;
8. an NDJSON export of all cases, and a dry run plus apply of a generated 20,000-case TestRail CSV into a
   second project (`<key>-imp`).

It prints p50, p95 and maximum latency per operation next to the targets, phase durations and the first
errors; the exit code is 1 only when requests failed, not when a target is missed. Both projects are deleted
at the end unless `--keep` is given. Every count has an option (`fastproof bench tm --help`); the data is
generated from `--seed` while it is sent, so the client stays small (about 30 MiB).

```bash
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_TOKEN_FILE=admin-token fastproof bench tm
```

The testers work flat out by default, which is a stress test: 50 testers then write about 1,500 step
results per second. `--think-ms 200` gives each tester a pause of 0 to 400 ms before every write, about 250
writes per second, still far busier than 50 people.

| Operation (20,000 cases) | Target p95 | Flat out | `--think-ms 200` |
|---|---|---|---|
| List page (50) with 2 filters | < 200 ms | 177 to 190 ms | 174 ms |
| Tree level with counts | < 150 ms | 4 ms | 3 ms |
| Save a case with 100 steps (new version) | < 150 ms | 22 to 57 ms | 23 ms |
| Write a step result (50 testers) | < 50 ms | 89 to 93 ms | 5 ms |
| Final status with projection | < 150 ms | 342 to 486 ms | 15 ms |
| Launch from a plan of 5,000 cases | < 3 s | 81 to 87 ms | (same phase) |
| Export 23,000 cases as NDJSON | < 20 s | 1.8 to 2.5 s | |
| Import 20,000 TestRail CSV cases (dry run + apply) | < 60 s | 13 to 14 s | |

Measured with the release build on one development VM, the server limited to 4 CPUs and sharing the machine
with PostgreSQL 18 and the bench; the server's peak memory was about 180 MiB. When testers work flat out,
step writes and final statuses of one launch wait on each other's locks on the launch row, so their latency
grows with the write rate of a single launch rather than with the data size.
