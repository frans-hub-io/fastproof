# Contributing to FastProof

Thank you for helping. Issues and pull requests are welcome; for larger changes, open an issue first so we can
agree on the approach.

## Setup

Check out `franshub-platform` next to this repository (the Rust crates and the UI use it through relative
paths), then:

```sh
cd franshub-platform/packages/ui-shell && npm ci && npm run build
cd ../../../fastproof
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
FASTPROOF_TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test --workspace
cd ui && npm ci && npm run lint && npm run typecheck && npm test && npm run build
```

Integration tests run when the `FASTPROOF_TEST_*` variables point to PostgreSQL, an S3-compatible server
(Garage works well) and, optionally, Valkey; see `.github/workflows/ci.yml` for the exact setup.

The server tests start a real server, and its scheduler runs every periodic job (sweeps, retention, media
preparation, backfills and so on) at start and on its schedule. A test that does a job's work itself (calls
its function), ages rows that a job acts on, or checks a state a job changes starts the server with those
jobs paused: `Server::start_paused(&db, &["retention.apply"])` (see `crates/fastproof-server/tests/common`).
A paused job runs only when the test makes it due with `db.run_scheduled("retention.apply")`, which
exercises the job through the real scheduler. Wait for a state rather than sleeping a fixed time.

Helm chart checks:

```sh
helm dependency build deploy/helm/fastproof
helm lint --strict deploy/helm/fastproof -f deploy/helm/fastproof/ci/minimal-fs-values.yaml
helm unittest deploy/helm/fastproof
```

Documentation: `docs/prepare.sh && mkdocs build --strict` with `mkdocs-material` installed.

## Releasing

1. Set the next version in `[workspace.package]` of `Cargo.toml` (for example `0.2.0-dev1`; `cargo build`
   updates `Cargo.lock`), the chart's `appVersion` in `deploy/helm/fastproof/Chart.yaml` and the version named
   in `README.md` and the docs, and merge that to `main`.
2. Tag the merge commit `v<version>` and push the tag. The release workflow builds the image with
   `VERSION=<version>` (the tag without `v`).
3. Check the version the image reports: `docker run --rm docker.io/franshub/fastproof:<version> version`.

Where the version comes from: the image build passes `VERSION` to the compiler as `FASTPROOF_BUILD_VERSION`,
which becomes the version of the binary (`fastproof version` and `--version`, `GET /api/v1/version`, the UI
footer, `fastproof_build_info{version}` on `/metrics`, the MCP server info and the `User-Agent` of outbound
calls). A build without `FASTPROOF_BUILD_VERSION` (`cargo build`, CI) reports the Cargo version from step 1.

## Guidelines

- Keep pull requests focused, with tests for new behaviour and a short description of why.
- Database migrations are additive: no dropping or renaming of columns still used by the previous release,
  indexes with `CREATE INDEX CONCURRENTLY`, explicit column lists in queries.
- Every parser needs limits; uploaded content must never be executed.
- Update the documentation in `docs/` with user-visible changes.
- Use example values in code and docs: `example.com` host names, `198.51.100.0/24` and `203.0.113.0/24`
  addresses.
- Commit messages: one line in the imperative mood.

By contributing you agree that your contributions are licensed under the Apache License 2.0.
