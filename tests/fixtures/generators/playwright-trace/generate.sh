#!/usr/bin/env bash
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SRC/../../../.." && pwd)"
OUT="$REPO/tests/fixtures/playwright-trace"
WORK="${WORK:-$(mktemp -d)}"
PROJ="$WORK/fastproof-samples"
CI_PATH="/builds/example/fastproof-samples"

export npm_config_cache="${npm_config_cache:-$WORK/npm-cache}"
export CI=1

rm -rf "$PROJ"
mkdir -p "$PROJ"
cp "$SRC/package.json" "$SRC/playwright.config.ts" "$PROJ/"
cp -R "$SRC/tests" "$PROJ/tests"
cd "$PROJ"
npm install --no-audit --no-fund --no-package-lock
npx playwright install chromium
npx playwright test

mkdir -p "$OUT" "$WORK/trace"
find "$OUT" -mindepth 1 -maxdepth 1 ! -name README.md -exec rm -rf {} +
unzip -o -q test-results/*/trace.zip -d "$WORK/trace"
cp "$WORK/trace"/*.trace "$OUT/"

PHYS="$(cd "$PROJ" && pwd -P)"
export PHYS PROJ CI_PATH
perl -pi -e 's/\Q$ENV{PHYS}\E/$ENV{CI_PATH}/g; s/\Q$ENV{PROJ}\E/$ENV{CI_PATH}/g;' "$OUT"/*.trace

if grep -Il -e '/tmp/' -e '/Users/' -e '/home/' "$OUT"/*.trace; then
  echo "unsanitized values remain in the files above" >&2
  exit 1
fi
echo "fixtures written to $OUT"
