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

Helm chart checks:

```sh
helm dependency build deploy/helm/fastproof
helm lint --strict deploy/helm/fastproof -f deploy/helm/fastproof/ci/minimal-fs-values.yaml
helm unittest deploy/helm/fastproof
```

Documentation: `docs/prepare.sh && mkdocs build --strict` with `mkdocs-material` installed.

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
