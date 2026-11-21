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

Topics (after `FASTPROOF_KAFKA_TOPIC_PREFIX`, default `fastproof.`): `ingest.results`, `ingest.logs`,
`ingest.uploads` and `events`. Create them yourself with the partition count you want, or let FastProof create
them (`kafka.topics.create`, on for the bundled Kafka).

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
| Large, Kafka | api 2+, ingest 2+, writer 2+, sink 1-2 | HA cluster | 3 brokers | 2+ replicas | 3+ nodes |

- Kafka partitions bound writer parallelism: run at most as many writer replicas as partitions of
  `ingest.results`.
- Size the PostgreSQL connections for all Deployments: replicas × `database.maxConnections` summed over roles.
- Run one heavy backend at a time on small nodes: ClickHouse, OpenSearch and Kafka together need about
  4.5 GiB.

!!! note "TODO"
    Throughput and lag numbers from the big-data benchmark (G9) will be added here.

## Benchmark

`fastproof bench` generates launches, results and log lines with realistic failure patterns, pushes them
through the HTTP API (or straight to Kafka), and reports throughput and server-side lag. Run it as a
Kubernetes Job next to the installation you want to measure.
