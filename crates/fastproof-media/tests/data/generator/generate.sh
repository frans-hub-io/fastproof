#!/usr/bin/env bash
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$(cd "$SRC/.." && pwd)/playwright"
WORK="${WORK:-$(mktemp -d)}"
PROJ="$WORK/fastproof-samples"
CI_PATH="/builds/example/fastproof-samples"

export npm_config_cache="${npm_config_cache:-$WORK/npm-cache}"
export CI=1

rm -rf "$PROJ"
mkdir -p "$PROJ"
cp "$SRC/package.json" "$SRC/playwright.config.mjs" "$PROJ/"
cp -R "$SRC/tests" "$PROJ/tests"
cd "$PROJ"
npm install --no-audit --no-fund --no-package-lock
npx playwright install chromium-headless-shell ffmpeg
npx playwright test

rm -rf "$OUT"
mkdir -p "$OUT"
cp results.json "$OUT/results.json"
(cd test-results && find . -name '*.webm' -print0 | while IFS= read -r -d '' f; do
  mkdir -p "$OUT/test-results/$(dirname "$f")"
  cp "$f" "$OUT/test-results/$f"
done)

PHYS="$(cd "$PROJ" && pwd -P)"
OS_USER="$(id -un)"
export PHYS PROJ CI_PATH OS_USER
perl -pi -e '
  s/\Q$ENV{PHYS}\E/$ENV{CI_PATH}/g;
  s/\Q$ENV{PROJ}\E/$ENV{CI_PATH}/g;
  s{/(home|Users)/\Q$ENV{OS_USER}\E\b}{/home/runner}g;
' "$OUT/results.json"

if grep -Il -e '/tmp/' -e '/Users/' -e '/home/' "$OUT/results.json"; then
  echo "unsanitized values remain in results.json" >&2
  exit 1
fi
find "$OUT" -type f -exec ls -l {} +
echo "fixtures written to $OUT"
