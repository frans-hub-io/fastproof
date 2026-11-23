ARG NODE_IMAGE=docker.io/library/node:24.22.3-trixie-slim@sha256:af02d57b57719b9657a45199a79264a9437940cd55b3c7b3fc99615654f124a0
ARG RUST_IMAGE=docker.io/library/rust:1.99.1-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7
ARG RUNTIME_IMAGE=gcr.io/distroless/static-debian13:nonroot@sha256:25ac8f0c3b2334a9eb05452266bd4697864a8c8ef0ef71e5e6480d7f92868329

FROM --platform=$BUILDPLATFORM ${NODE_IMAGE} AS ui
ENV CI=true \
    npm_config_audit=false \
    npm_config_fund=false \
    npm_config_update_notifier=false
WORKDIR /src/franshub-platform/packages/ui-shell
COPY franshub-platform/packages/ui-shell/package.json franshub-platform/packages/ui-shell/package-lock.json ./
RUN npm ci
COPY franshub-platform/packages/ui-shell/ ./
RUN npm run build
WORKDIR /src/fastproof/ui
COPY fastproof/ui/package.json fastproof/ui/package-lock.json ./
RUN npm ci
COPY fastproof/ui/ ./
RUN npm run build

FROM ${RUST_IMAGE} AS build
ARG TARGETARCH
ARG VERSION=
RUN apt-get update \
 && apt-get install -y --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/*
RUN case "${TARGETARCH}" in \
      amd64) triple=x86_64-unknown-linux-musl ;; \
      arm64) triple=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
 && rustup target add "${triple}" \
 && echo "${triple}" > /target-triple
COPY franshub-platform/Cargo.toml franshub-platform/Cargo.lock /src/franshub-platform/
COPY franshub-platform/crates /src/franshub-platform/crates
COPY fastproof/ /src/fastproof/
COPY --from=ui /src/fastproof/ui/dist /src/fastproof/ui/dist
WORKDIR /src/fastproof
ENV CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_RELEASE_DEBUG=0 \
    CARGO_TERM_COLOR=never
RUN triple="$(cat /target-triple)" \
 && FASTPROOF_BUILD_VERSION="${VERSION}" \
    RUSTFLAGS="--remap-path-prefix=${CARGO_HOME}=/cargo --remap-path-prefix=/src=/build --remap-path-prefix=/src/fastproof/target=/target" \
    cargo build --release --locked --target "${triple}" -p fastproof-server --bin fastproof \
 && install -D -m 0755 "target/${triple}/release/fastproof" /out/usr/local/bin/fastproof \
 && install -d -m 0750 /out/var/lib/fastproof/blobs

FROM ${RUNTIME_IMAGE}
ARG VERSION=0.2.0-dev1
ARG REVISION=unknown
LABEL org.opencontainers.image.title="FastProof" \
      org.opencontainers.image.description="Self-hosted test reporting server: allure-results, JUnit XML, CTRF and Playwright reports with history, flaky detection and triage" \
      org.opencontainers.image.url="https://fastproof.franshub.io/" \
      org.opencontainers.image.documentation="https://fastproof.franshub.io/" \
      org.opencontainers.image.source="https://github.com/frans-hub-io/fastproof" \
      org.opencontainers.image.vendor="FransHub" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}"
COPY --from=build /out/usr/local/bin/fastproof /usr/local/bin/fastproof
COPY --from=build --chown=65532:65532 /out/var/lib/fastproof /var/lib/fastproof
USER 65532:65532
EXPOSE 8080 9090
ENTRYPOINT ["/usr/local/bin/fastproof"]
CMD ["serve"]
