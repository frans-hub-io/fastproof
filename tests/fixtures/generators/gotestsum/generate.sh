#!/usr/bin/env bash
set -euo pipefail

GEN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$GEN_DIR/../../../.." && pwd)"
OUT="$REPO_ROOT/tests/fixtures/gotestsum"
GOTESTSUM_VERSION="v1.13.0"
WORK="${WORK:-$(mktemp -d)}"
WORK="$(cd "$WORK" && pwd)"
PROJ="$WORK/fastproof-samples"
GO_CACHE_DIR="${GO_CACHE_DIR:-$WORK/go-cache}"
HOST="$(hostname)"
USER_NAME="$(id -un)"
SANDBOX_PATH="/builds/example/fastproof-samples"

export GOPATH="$GO_CACHE_DIR/gopath"
export GOMODCACHE="$GO_CACHE_DIR/gopath/pkg/mod"
export GOCACHE="$GO_CACHE_DIR/build"
export GOFLAGS="-modcacherw"
export GOTOOLCHAIN="local"
export XDG_CONFIG_HOME="$GO_CACHE_DIR/config"
mkdir -p "$GOPATH" "$GOCACHE" "$XDG_CONFIG_HOME"

GO_BIN="${GO:-go}"
GOROOT_DIR="$("$GO_BIN" env GOROOT)"
"$GO_BIN" version

GOTESTSUM="${GOTESTSUM:-$GOPATH/bin/gotestsum}"
if [ ! -x "$GOTESTSUM" ]; then
  "$GO_BIN" install "gotest.tools/gotestsum@$GOTESTSUM_VERSION"
  GOTESTSUM="$GOPATH/bin/gotestsum"
fi
export PATH="$GOROOT_DIR/bin:$PATH"
"$GOTESTSUM" --version

rm -rf "$PROJ"
mkdir -p "$PROJ"
cp -R "$GEN_DIR/go.mod" "$GEN_DIR/checkout" "$GEN_DIR/payments" "$GEN_DIR/inventory" "$PROJ/"

export FASTPROOF_COUNTER_FILE="$WORK/attempts.json"
cd "$PROJ"

rm -f "$FASTPROOF_COUNTER_FILE" junit.xml junit-norerun.xml
set +e
"$GOTESTSUM" --format standard-verbose --junitfile junit.xml --rerun-fails=2 --packages="./checkout ./payments" -- -count=1
rc1=$?
rm -f "$FASTPROOF_COUNTER_FILE"
"$GOTESTSUM" --format standard-verbose --junitfile junit-norerun.xml --packages=./... -- -count=1
rc2=$?
set -e
echo "gotestsum exit codes: $rc1 $rc2"

rm -f "$OUT/junit.xml" "$OUT/junit-norerun.xml"
mkdir -p "$OUT"
cp junit.xml junit-norerun.xml "$OUT/"

for f in "$OUT/junit.xml" "$OUT/junit-norerun.xml"; do
  sed -i.bak \
    -e "s#$PROJ#$SANDBOX_PATH#g" \
    -e "s#$GOROOT_DIR#/usr/local/go#g" \
    -e "s#$WORK#/builds/example#g" \
    -e "s#/home/$USER_NAME#/home/runner#g" \
    -e "s#$HOST#runner.example.com#g" \
    "$f"
  rm -f "$f.bak"
done

if grep -Il -e '/tmp/' -e '/Users/' -e '/home/' -e "$HOST" "$OUT/junit.xml" "$OUT/junit-norerun.xml"; then
  echo "sanitization left private paths or host names in the files above" >&2
  exit 1
fi

echo "outputs written to $OUT"
