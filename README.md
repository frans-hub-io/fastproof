# FastProof

FastProof is an open-source (Apache-2.0), self-hosted test reporting server. CI uploads the reports it already
writes (allure-results, JUnit XML, CTRF, Playwright JSON or the FastProof JSON format); FastProof keeps every
launch with history per test, using Allure-compatible test identity, detects flaky tests, groups failures and
helps the team triage them. One Rust binary serves the API, the ingest queue and the web UI, with PostgreSQL
and S3-compatible storage (or a directory); Valkey is optional. Large installations can add Kafka between the
API and the database, run the roles as separate Deployments, and copy results and logs to ClickHouse or
OpenSearch for search.

Status: early development (`0.1.0-dev2`).

- Documentation: <https://fastproof.franshub.io/>
- Image: [`franshub/fastproof`](https://hub.docker.com/r/franshub/fastproof)
- Helm chart: [`deploy/helm/fastproof`](deploy/helm/fastproof)

## Quick start

```sh
export FASTPROOF_TOKEN="$(openssl rand -hex 32)"
printf 'admin:admin:sha256:%s\n' "$(printf %s "$FASTPROOF_TOKEN" | sha256sum | cut -d' ' -f1)" > admin-tokens
chmod 0644 admin-tokens

docker network create fastproof
docker run -d --name fastproof-db --network fastproof \
  -e POSTGRES_USER=fastproof -e POSTGRES_PASSWORD=fastproof -e POSTGRES_DB=fastproof \
  docker.io/library/postgres:18.7-trixie
docker run -d --name fastproof --network fastproof -p 127.0.0.1:8080:8080 -p 127.0.0.1:9090:9090 \
  -e FASTPROOF_DATABASE_URL=postgres://fastproof:fastproof@fastproof-db:5432/fastproof \
  -e FASTPROOF_ADMIN_TOKENS_FILE=/etc/fastproof/admin-tokens \
  -v "$PWD/admin-tokens:/etc/fastproof/admin-tokens:ro" -v fastproof-blobs:/var/lib/fastproof \
  docker.io/franshub/fastproof:0.1.0-dev2

curl -fsS -X POST http://localhost:8080/api/v1/projects -H "Authorization: Bearer $FASTPROOF_TOKEN" \
  -H 'Content-Type: application/json' -d '{"key":"web","name":"Web shop"}'
```

Then upload a report with the CLI (the same binary) and open <http://localhost:8080/>. See
[Getting started](https://fastproof.franshub.io/guide/getting-started/) and
[CI recipes](https://fastproof.franshub.io/guide/ci-recipes/).

## Repository layout

| Path | Contents |
|---|---|
| `crates/fastproof-model` | Result model, Allure-compatible identity, retries and flaky detection |
| `crates/fastproof-ingest` | Bounded streaming parsers for allure-results, JUnit XML, CTRF and Playwright JSON |
| `crates/fastproof-server` | The `fastproof` binary: API, ingest queue, UI, CLI |
| `ui/` | Web UI (TypeScript, React, Mantine) |
| `schemas/` | JSON Schema of the FastProof report format |
| `deploy/helm/fastproof` | Helm chart |
| `docs/` | Documentation site (MkDocs Material) |
| `tests/fixtures` | Reports produced by the real writers, with their generators |

## Building

The build uses the shared `franshub-platform` repository checked out next to this one:

```text
parent/
  fastproof/
  franshub-platform/
```

```sh
cd parent
docker build -f fastproof/Dockerfile -t fastproof:dev .
```

The build context is the parent directory; `fastproof/Dockerfile.dockerignore` limits it to the two
repositories' sources. For development: `cargo test --workspace` in `fastproof/`, and `npm ci && npm run dev`
in `fastproof/ui/` after building `franshub-platform/packages/ui-shell`.

## Contributing and security

See [CONTRIBUTING.md](CONTRIBUTING.md). Report vulnerabilities privately as described in
[SECURITY.md](SECURITY.md).

## License

Apache License 2.0, see [LICENSE](LICENSE).
