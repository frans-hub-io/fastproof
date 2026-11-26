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
| Ordering | per upload | per test (records are keyed by launch and test) |

In Kafka mode only the gateway produces, after token authentication, unless
`FASTPROOF_KAFKA_TRUSTED_PRODUCERS=true`. Consumer groups are coordinated by FastProof in PostgreSQL (Kafka
consumer groups are not used), and the writer commits its offset in the same transaction as the data, so
retries never duplicate results and a writer killed in the middle of a batch loses nothing: the next owner of
the partition starts again from the last committed offset. Result records are keyed by launch and test, and
log lines by launch and result, so a big launch spreads over every partition and every writer, while the
attempts of one test (and the lines of one result) share a partition and are stored in the order they were
produced. Results of different tests of a launch can be stored in another order than they were sent, and
results parsed from uploaded archives are stored together per upload. FastProof exports consumer lag on
`/metrics`.

Launches that run the same tests are written at the same time by several writers (or ingest workers) without
waiting on each other for long. Test cases and series that exist and need no change are only share-locked
(nobody can delete them meanwhile, and nobody waits for them); the series statistics, which every launch of
those tests updates, are locked last, right before they are written, so such launches wait for each other
only for that final step. When another launch of the same tests committed in the meantime, the history of
those tests is read again under the lock, so transitions and flaky verdicts follow the order in which the
launches commit. Several writers of one launch run side by side as well: the launch row is only
share-locked while they work and updated by their change of the counters right before the commit, and two
of them wait for each other only when they store the same test. Writers lock the results that exist in id
order and the launch row last, as triage, rule tasks and mute expiry do. Results a writer creates, and
results a running attempt leaves when it is completed under another identity, are locked later in the
transaction, so two writers can still meet in a deadlock in those cases (two writers creating the same
results, a moved running attempt); such a transaction, and one that finds that another writer created the
same result meanwhile, runs again after a short pause (counted in `fastproof_db_tx_retries_total`).
Deleting a launch, or closing its manual run, waits only for the writers in flight: new writers of that
launch queue behind it.

Topics (after `FASTPROOF_KAFKA_TOPIC_PREFIX`, default `fastproof.`): `ingest.results`, `ingest.logs`,
`ingest.uploads` and `events`. FastProof produces them with Snappy compression and reads any codec; a
consumer of `events` outside FastProof needs a Kafka client with Snappy. Create them yourself with the partition count you want, or let FastProof create
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
| Lab big data (`ci/bigdata-values.yaml`) | api, ingest, writer, sink at 0.5-1 CPU, 384-768 MiB, pools of 12 | 1 CPU, 1 GiB | 1 broker, 768 MiB (384 MiB heap) | 1 node, 2 GiB | none |
| Large, Kafka | api 2+, ingest 2+, writer 2+, sink 1-2 | HA cluster | 3 brokers over TLS (managed, or `kafka.replicaCount: 3`) | 2+ replicas | 3+ nodes |

- Kafka partitions bound writer parallelism: run at most as many writer replicas as partitions of
  `ingest.results`.
- Size the PostgreSQL connections for all Deployments: replicas × `database.maxConnections` summed over roles
  (see below).
- Run one heavy backend at a time on small nodes: ClickHouse, OpenSearch and Kafka together need about
  4.5 GiB.

## PostgreSQL memory and connections

Every PostgreSQL connection is a process with its own memory: an idle FastProof connection holds 4 to 6 MiB,
a busy one more (`work_mem` per sort or hash, the parameters of bulk inserts). Pools fill under load and stay
open while busy, so size PostgreSQL for the connections the pods can open, not for the ones open at rest:

- **Connections.** Each pod opens up to `database.maxConnections` (`FASTPROOF_DATABASE_MAX_CONNECTIONS`, 20 by
  default) plus one, the migration Job one more. With roles split and the bundled Kafka and a sink, the
  defaults (api 2, ingest 1, writer 1, sink 1) come to 5 × 21 + 1 = 106; at the most replicas of the
  autoscalers it is more. `max_connections` must cover that plus the 3 connections PostgreSQL keeps for
  superusers. Rolling updates add one pod per Deployment for a short while.
- **Memory.** Plan for `shared_buffers` + 8 MiB per connection + 3 autovacuum workers × `maintenance_work_mem`
  + 128 MiB, and leave the rest of the limit to the page cache. The kernel counts the page cache in the
  container's memory too, but takes it back under pressure; the processes' own memory it cannot take back,
  and a PostgreSQL over its limit is killed with every connection.
- **Settings.** `shared_buffers` about a sixth to a quarter of the limit, `work_mem` 4 MB,
  `maintenance_work_mem` 64 MB (128 MB from 2 GiB), `effective_cache_size` about half the limit.

| PostgreSQL memory limit | `shared_buffers` | Connections it carries | Example |
|---|---|---|---|
| 1 GiB | 192MB | 64 | 4 pods with `database.maxConnections: 12` (`ci/bigdata-values.yaml`) |
| 1.5 GiB (bundled default) | 256MB | 120 | 5 pods with the default pools of 20 |
| 2 GiB | 512MB | 128 | 6 pods with pools of 20 |
| 4 GiB | 1GB | 320 | 15 pods with pools of 20 |

The chart checks both for the bundled PostgreSQL and fails the install or upgrade with the numbers when the
pools exceed `postgresql.config.postgresql.max_connections` or the estimate exceeds
`postgresql.resources.limits.memory` (`database.sizingCheck: false` skips it); for CloudNativePG it checks the
connections against `cnpg.cluster.postgresql.parameters.max_connections`. Writers need about one connection
per partition they hold at once plus a few; a sink or ingest pod needs fewer than an API pod, so lowering
`database.maxConnections` is usually the cheaper fix. Under the 100,000-result bench a PostgreSQL of 768 MiB
serving six pods with pools of 20 was killed for memory; the bigdata profile now gives it 1 GiB with pools of 12.

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

`fastproof bench --launches 100 --results 1000 --log-lines 20` through the HTTP API: 103,346 results (retries
included) and 2,066,920 log lines, 100 launches running the same 1,000 tests, 4 launches sent at a time.

| Path | Results per second sent | Log lines per second sent | Lag after the last request | Until everything is stored |
|---|---|---|---|---|
| `direct`, one process without a CPU limit | 8,100 to 8,400 | 161,000 to 169,000 | none beyond the request | 12 to 13 s |
| `kafka`, gateway plus two writers of half a CPU each, 6 partitions | 11,500 to 12,800 | 230,000 to 256,000 | 10 to 13 s | 19 to 22 s |

In Kafka mode the gateway answers once the records are in Kafka, so clients finish sooner and the writers
catch up afterwards; the lag is what is left of the backlog when the last request returns, so faster senders
see a longer lag for the same work. The writers were limited by their CPU here: more writers (up to the
partition count) or more CPU per writer shorten it.

### Kafka writer

Where the time of the bench above went before, and what changed:

- **Log lines:** three statements per result and attempt (look for stored chunks, insert the chunk, insert
  its outbox event), about 330,000 statements in all. Now a batch takes three statements whatever the number
  of results it holds.
- **Launches sharing tests:** storing results upserted the shared test cases and series, and an upsert locks
  the rows it matches until commit even when it changes nothing, so the results of all launches were written
  one transaction at a time. Unchanged rows are now only share-locked, and the series statistics are locked
  as the last step (re-reading the history of the tests another launch changed meanwhile).
- **History of a series** (flaky detection, transitions): the query joined every past result to its launch,
  which PostgreSQL ran as a probe of every launch of the branch for every series while its statistics were
  not fresh (200 ms per batch). It now reads the latest results of each series and checks their few launches
  afterwards.
- **Writer CPU:** the gateway signature of every record keyed its MAC from scratch and decoded hex slowly
  (a fifth of the writer's CPU), and LZ4 batches were parsed through a streaming decoder (an eighth). The MAC
  state is now prepared once, and records are produced with Snappy, which rskafka parses from memory.
- **Gateway:** the log byte budget of a launch was kept on the launch row, which every results transaction of
  the launch locks, so log requests waited for the writers; it has a row of its own now.

| Same bench, two writers of half a CPU each | Before | After |
|---|---|---|
| Lag after the last request | 25 to 29 s | 10 to 13 s |
| From the first request until everything is stored | 38 to 43 s | 19 to 22 s |
| CPU of both writers | 28 to 31 s | 20 s |
| PostgreSQL CPU spent on the writers' statements | 47 to 61 s | 24 to 26 s |
| Peak memory of a writer | 130 to 160 MiB | 170 to 230 MiB |
| `direct` mode, until everything is stored | 27 s | 13 s |

Every run stored every result and log line exactly once. Measured with the release build on one development
VM shared with other work, PostgreSQL 18 on the same VM, the writers held to half a CPU each as
`ci/bigdata-values.yaml` limits `roles.writer`. PostgreSQL needs about as much CPU time as the table shows
for the writers' statements, so a PostgreSQL with one CPU is busy for most of such a burst: size it together
with the writers.

#### One big launch

A single launch of 100,000 results used to go through one partition, one writer and one transaction at a
time (each held the launch row to its end and recounted the whole launch), so it was stored at the speed
of one writer whatever the number of writers. Its records are now keyed by launch and test, every writer
takes part, the counters change by what each transaction changed, and the launch row is held only for
that final update.

| `fastproof bench --launches 1 --results 100000 --log-lines 2 --parallel 1`, two writers of half a CPU each, 6 partitions | Before | After |
|---|---|---|
| Lag after the last request | 44 to 59 s | 1 to 6 s |
| From the first request until everything is stored | 66 to 109 s | 37 to 64 s (mostly the sending) |
| Records per writer | all on one writer | half on each |
| PostgreSQL CPU of the writers | 51 to 60 s | 57 to 82 s |

PostgreSQL works more for the same launch, because its writers now meet on the launch row (the foreign
key checks of every stored row read it); spread over the writers, that still stores the launch several
times faster. The 100-launch bench above is unchanged within the noise of the shared VM. Every run stored
every result and log line exactly once.

The statistics lock: `fastproof_analysis_stats_lock_wait_seconds` is the wait for the statistics rows
of a batch, `fastproof_analysis_stats_locked_seconds` the work done while holding them. In the 100-launch
bench a batch waits 50 to 75 ms and holds them 13 to 18 ms; series that another launch changed are judged
again before the lock when it is seen in time (a quarter of them,
`fastproof_analysis_rechecked_series_total{phase="before_lock"}`), the others under it.

#### Outbox volume

`FASTPROOF_OUTBOX_LOG_EVENTS=batch` writes one `log.chunks` event per launch and write instead of one
`log.chunk` event per stored chunk: the 100-launch bench writes about 204,000 outbox rows instead of
309,000. The sinks read both formats; see [Upgrading](upgrading.md) before switching.

### Launch explorer

The Suites tree of a finished launch of 100,000 results (`fastproof bench --launches 2 --results 100000
--log-lines 0`, 20 suites, one of them inlined with its 5000 tests), as the launch page asks for it first;
release build, 60 requests each, load average 1 to 3:

| First view | p50 | p95 |
|---|---|---|
| Built in the background when the launch finished (`explorer.warm`), read from the cache | 36 ms | 53 ms |
| Built on the request (a running launch, or the cache removed before every request) | 100 to 140 ms | 145 to 160 ms |

At a load average of 3, building on the request took 109 to 143 ms (p50) and 278 to 316 ms (p95) with the
suite index, 149 to 169 ms and 419 to 599 ms without it. Most of that time is the count over the launch's
rows (about 75 ms when they are in memory); the index makes reading the inlined tests independent of the
launch size.

What the suite index costs when results are stored (`fastproof bench --launches 10 --results 2000`, six
rounds each, alternating): without log lines 5,700 results per second with the index and 5,900 without
(medians 5,700 and 6,300, rounds ranging from 4,600 to 6,500); with 20 log lines per result 3,900 and
3,700, within the noise.

## Benchmark

`fastproof bench` generates launches, results and log lines with realistic failure patterns, pushes them
through the HTTP API (or straight to Kafka), and reports throughput and server-side lag. Run it as a
Kubernetes Job next to the installation you want to measure.

### Analysis

`fastproof bench analysis` measures FastProof Analysis through the public API with an admin token. Profile
`s` (the default) uploads synthetic launches, triages them, and times the suggestions of a launch of 5,000
results in 300 clusters (targets: the `suggestions` step < 5 s, `GET …/suggestions` p95 < 100 ms). Profile
`m` also times every analytics endpoint over a 90-day window and the portfolio (targets with ClickHouse:
each endpoint p95 < 1 s, the portfolio p95 < 3 s). Measure profile `m` on a project that already holds the
target volume (about 100 million results and a billion log lines over 90 days), with the ClickHouse sink on
and the analysis role running:

```bash
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_TOKEN_FILE=admin-token \
  fastproof bench analysis --profile m --analytics-only --analytics-project big-project --analytics-days 90
```

Without `--analytics-only` the bench first creates its own project and runs profile `s`, then measures the
analytics of that project (or of `--analytics-project`). The report lists p50 and p95 per endpoint (and a JSON line) and the
backend that answered; the exit code does not depend on the targets.

| Profile `m` (ClickHouse, 100 M results, 90 days) | Target p95 | Measured |
|---|---|---|
| Analytics endpoints of one project | < 1 s | not measured yet |
| Portfolio | < 3 s | not measured yet |

### Test management

`fastproof bench tm` loads test management through the public API only, with the same `FASTPROOF_URL` and
token as `fastproof bench` (an admin token: it creates and deletes projects). It creates a project with a
random key (or `--project`), then:

1. 30 custom fields of mixed types and a folder tree of 300 folders up to 5 levels deep, then 500
   [shared steps](../guide/shared-steps.md);
2. 20,000 cases with 12 steps on average (some nested), random fields and tags, 4 more versions of 2,000 of
   them (`PUT` with `If-Match`) and 200 saves of a case with 100 steps. The plain cases reference the shared
   steps, two per case on average, and 5,000 of them the same one (the hot step); 2,000 cases are
   [parameterized](../guide/parameters.md) with 8 rows each;
3. an allure-results upload of 5,000 tests on `main`, 2,000 of them with `ALLURE_ID` labels naming existing
   cases, and waits until code sync has linked them;
4. 200 samples each of a list page (50) with two filters, a tree level with counts and a case;
5. a managed plan of 5,000 cases (2,000 manual cases by a tag filter, the rest automated by number) and a
   launch started from it;
6. 50 virtual testers executing the 2,000 manual run items (25 steps each, 50,000 step results), each step
   written separately, then a final status;
7. the automated part uploaded into the plan launch, and the manual run closed;
8. an NDJSON export of all cases, and a dry run plus apply of a generated 20,000-case TestRail CSV into a
   second project (`<key>-imp`);
9. the night-2 phase: 3,000 [requirements](../guide/requirements.md) (60 epics, 300 features, the rest stories)
   with 25,000 links to cases; coverage pages of 50 requirements in the default scope; 20 FPQL queries of 4 to
   6 conditions, each listed (page of 50) and validated with its count; ten saves of the hot shared step; a
   second managed plan of 5,000 cases (the 2,000 parameterized ones by a tag filter, the rest by number) and a
   launch from it (16,000 iterations, shared steps pinned), the probe case with 5 shared steps and 20
   iterations opened and its iteration steps written, and the coverage page of the plan scope; then review is
   switched on, 500 cases are edited (each becomes pending), the review queue is read and the 500 versions
   are approved in one bulk request (review is switched off again).

It prints p50, p95 and maximum latency per operation next to the targets, phase durations and the first
errors; the exit code is 1 only when requests failed, not when a target is missed. Both projects are deleted
at the end unless `--keep` is given. Every count has an option (`fastproof bench tm --help`); the data is
generated from `--seed` while it is sent, so the client stays small (about 30 MiB).

Requirements are linked with thousands of calls that take the per-caller write rate, so run the server under
test with `FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE=100000`, and with `FASTPROOF_TM_GATE=open` on an
installation of one replica (parameters and shared steps are gated formats, see
[Upgrading](upgrading.md#next-release-test-management-gate-and-review)).

!!! warning "Only on a server of its own"
    Run `bench tm` against a server and database set up for the bench, never against an installation people
    or CI use: it writes tens of thousands of cases, links and results, and the settings above switch off
    protections for every caller of that server. The write rate is what stops a runaway script or a
    prompt-injected agent; keep `FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE` at its default (or near it) on
    production, and never set `FASTPROOF_TM_GATE=open` on an installation with more than one replica. The night-2 counts have options
(`--shared-steps`, `--shared-hot-cases`, `--parameterized`, `--parameter-rows`, `--requirements`,
`--requirement-links`, `--fpql-queries`, `--n2-plan-cases`, `--review-votes`); `--shared-steps 0` leaves the
night-2 data out. The report also prints the load average (`/proc/loadavg`) of the machine the bench ran on
before and after the run, which says how far to trust the numbers when the server shares the machine.

```bash
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_TOKEN_FILE=admin-token fastproof bench tm
```

The testers work flat out by default, which is a stress test: 50 testers then write about 2,900 step
results per second. `--think-ms 200` gives each tester a pause of 0 to 400 ms before every write, about 250
writes per second, still far busier than 50 people.

| Operation (20,000 cases) | Target p95 | Flat out | `--think-ms 200` |
|---|---|---|---|
| List page (50) with 2 filters | < 200 ms | 177 to 190 ms | 174 ms |
| Tree level with counts | < 150 ms | 4 ms | 3 ms |
| Save a case with 100 steps (new version) | < 150 ms | 22 to 57 ms | 23 ms |
| Write a step result (50 testers) | < 50 ms | 20 ms | 3 ms |
| Final status with projection | < 150 ms | 58 to 66 ms | 12 ms |
| Launch from a plan of 5,000 cases | < 3 s | 81 to 87 ms | (same phase) |
| Export 23,000 cases as NDJSON | < 20 s | 1.8 to 2.5 s | |
| Import 20,000 TestRail CSV cases (dry run + apply) | < 60 s | 13 to 14 s | |

Night-2 operations in the same run (20,000 cases, 500 shared steps, 2,000 parameterized cases of 8 rows, 3,000
requirements with 25,000 links; testers flat out):

| Operation | Target p95 | Measured p95 |
|---|---|---|
| List page (50) with an FPQL query of 4 to 6 conditions (20 queries) | < 150 ms (< 500 ms at 200,000 cases) | 11 ms |
| `validate_query` with the count (20 queries) | < 200 ms | 28 ms |
| Save the shared step used by 5,000 cases | < 100 ms | 6 ms |
| Launch from a plan of 5,000 cases with shared steps and 16,020 iterations | < 3 s | 1.4 s |
| Open a run item with 5 shared steps and 20 iterations | < 80 ms | 6 ms |
| Write an iteration step | < 50 ms | 3 ms |
| Coverage page of 50 requirements, default scope | < 300 ms | 145 ms |
| Coverage page of 50 requirements, plan scope | < 1 s | 176 ms |
| Review queue page of 50 | < 150 ms | 4 ms |
| Bulk approval of 500 versions | < 2 s | 412 ms |

These are the first measurements of these operations (release build, development VM of 10 CPUs: the server
pinned to 4 of them, the bench to four others, PostgreSQL 18 on the same VM; load average 1.3 before the run
and 5.6 after it, the bench's own load included). The run took 134 s and made 90,274 requests without an
error. Everything is far inside its target, so read the numbers as a baseline to compare later releases
with, not as a limit. Saving a shared step does not rewrite the cases that use it, so that save does not grow
with the number of users. Dispatching a plan to CI is not in the bench: its time is mostly the provider's, and
the bench has no CI to call.

The 200,000-case stress for a bench stand (the default server limit of 200,000 cases per project is raised,
and the rate limit for writes that multiply work is lifted for the run):

```bash
FASTPROOF_TM_MAX_CASES=400000 FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE=100000 FASTPROOF_TM_GATE=open \
  fastproof serve

FASTPROOF_URL=https://bench.example.com FASTPROOF_TOKEN_FILE=admin-token \
  fastproof bench tm --cases 200000 --folders 3000 --version-sample 20000 --save-sample 2000 \
  --samples 500 --parallel 32 --read-parallel 8 --no-export --wait-timeout 7200
```

`--no-export` skips the NDJSON export, which stops at 50,000 cases (`maxExportCases`). With 200,000 cases the
report holds the FPQL list to < 500 ms. Cases are created at about 500 a second, so plan for an hour or
less. The other counts stay at the 20,000-case scenario, since the targets for plans, runs, iterations and
requirements are defined at that size.

Measured with the release build on one development VM, the server limited to 4 CPUs and sharing the machine
with PostgreSQL 18 and the bench; the server's peak memory was about 180 MiB. A step write locks only its run
item, and a status change updates the launch counters by its own difference in one short update at the end
of its transaction, so testers of one launch wait only for each other's commits on the launch row, not for a
recount of the whole launch.
