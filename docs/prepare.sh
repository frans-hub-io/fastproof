#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
docs="$root/docs"
swagger_version="${SWAGGER_UI_VERSION:-5.34.1}"

vendor="$docs/api/swagger/vendor"
if [ ! -f "$vendor/swagger-ui-bundle.js" ] || [ "$(cat "$vendor/VERSION" 2>/dev/null || true)" != "$swagger_version" ]; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  npm pack --silent --pack-destination "$tmp" "swagger-ui-dist@${swagger_version}" >/dev/null
  tar -xzf "$tmp"/swagger-ui-dist-*.tgz -C "$tmp"
  mkdir -p "$vendor"
  cp "$tmp/package/swagger-ui.css" "$tmp/package/swagger-ui-bundle.js" "$tmp/package/LICENSE" "$vendor/"
  printf '%s\n' "$swagger_version" > "$vendor/VERSION"
fi

if [ -f "$root/schemas/openapi.json" ]; then
  cp "$root/schemas/openapi.json" "$docs/api/openapi.json"
elif [ -n "${FASTPROOF_OPENAPI:-}" ] && [ -f "$FASTPROOF_OPENAPI" ]; then
  cp "$FASTPROOF_OPENAPI" "$docs/api/openapi.json"
else
  printf '%s\n' '{"openapi":"3.1.0","info":{"title":"FastProof API","version":"unreleased","description":"The API description is generated from the server and published with the next build."},"paths":{}}' > "$docs/api/openapi.json"
fi

mkdir -p "$docs/schemas"
if [ -f "$root/schemas/fastproof-report-v1.json" ]; then
  cp "$root/schemas/fastproof-report-v1.json" "$docs/schemas/fastproof-report-v1.json"
else
  printf '%s\n' '{"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"https://fastproof.franshub.io/schemas/fastproof-report-v1.json","title":"FastProof report v1","description":"Placeholder: the schema is generated from fastproof-model and published with the next build."}' > "$docs/schemas/fastproof-report-v1.json"
fi
