#!/usr/bin/env bash
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SRC/../../../.." && pwd)"
OUT="$REPO/tests/fixtures/vitest"
WORK="${WORK:-$(mktemp -d)}"
PROJ="$WORK/fastproof-samples"
CI_PATH="/builds/example/fastproof-samples"

export npm_config_cache="${npm_config_cache:-$WORK/npm-cache}"
export CI=1

rm -rf "$PROJ"
mkdir -p "$PROJ"
cp "$SRC/package.json" "$SRC/vitest.config.ts" "$PROJ/"
cp -R "$SRC/tests" "$PROJ/tests"
cd "$PROJ"
npm install --no-audit --no-fund --no-package-lock --legacy-peer-deps

set +e
npx vitest run
status=$?
set -e
echo "vitest exited with $status"

mkdir -p "$OUT"
find "$OUT" -mindepth 1 -maxdepth 1 ! -name README.md -exec rm -rf {} +
cp -R allure-results "$OUT/allure-results"
cp junit.xml "$OUT/junit.xml"

PHYS="$(cd "$PROJ" && pwd -P)"
HOST_SHORT="$(hostname -s 2>/dev/null || hostname)"
HOST_FULL="$(hostname)"
OS_USER="$(id -un)"
export PHYS PROJ CI_PATH HOST_SHORT HOST_FULL OS_USER

find "$OUT" -type f ! -name '*.png' ! -name README.md -print0 | xargs -0 perl -pi -e '
  s/\Q$ENV{PHYS}\E/$ENV{CI_PATH}/g;
  s/\Q$ENV{PROJ}\E/$ENV{CI_PATH}/g;
  s/\Q$ENV{HOST_FULL}\E/runner.example.com/g;
  s/\Q$ENV{HOST_SHORT}\E/runner.example.com/g;
  s{/(home|Users)/\Q$ENV{OS_USER}\E\b}{/home/runner}g;
  s/\b\Q$ENV{OS_USER}\E@/runner@/g;
'

if grep -rIl -e '/tmp/' -e '/Users/' -e '/home/' -e "$HOST_SHORT" -e "$OS_USER@" "$OUT" --exclude=README.md; then
  echo "unsanitized values remain in the files above" >&2
  exit 1
fi
echo "fixtures written to $OUT"
