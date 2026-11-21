#!/usr/bin/env bash
set -euo pipefail

GEN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$GEN_DIR/../../../.." && pwd)"
OUT="$REPO_ROOT/tests/fixtures/pytest"
PYTHON="${PYTHON:-python3.11}"
WORK="${WORK:-$(mktemp -d)}"
WORK="$(cd "$WORK" && pwd)"
PROJ="$WORK/fastproof-samples"
VENV="${VENV:-$WORK/venv}"
HOST="$(hostname)"
USER_NAME="$(id -un)"
SANDBOX_PATH="/builds/example/fastproof-samples"

rm -rf "$PROJ"
mkdir -p "$PROJ"
cp -R "$GEN_DIR/tests" "$GEN_DIR/pytest.ini" "$GEN_DIR/requirements.txt" "$PROJ/"

if [ ! -x "$VENV/bin/python" ]; then
  "$PYTHON" -m venv "$VENV" 2>/dev/null || { rm -rf "$VENV"; "$PYTHON" -m venv --without-pip "$VENV"; }
fi
if "$VENV/bin/python" -m pip --version >/dev/null 2>&1; then
  PIP=("$VENV/bin/python" -m pip)
else
  PIP=("$PYTHON" -m pip --python "$VENV/bin/python")
fi
"${PIP[@]}" install --quiet --disable-pip-version-check --cache-dir "$WORK/pip-cache" -r "$PROJ/requirements.txt"

export FASTPROOF_COUNTER_FILE="$WORK/attempts.json"
export PYTHONDONTWRITEBYTECODE=1

cd "$PROJ"
rm -f "$FASTPROOF_COUNTER_FILE"
rm -rf allure-results junit-xunit2.xml junit-xunit1.xml
set +e
"$VENV/bin/python" -m pytest -p no:cacheprovider \
  --alluredir allure-results \
  --junitxml junit-xunit2.xml \
  -o junit_family=xunit2 \
  -o junit_logging=all \
  -o junit_suite_name=fastproof-samples
rc1=$?
rm -f "$FASTPROOF_COUNTER_FILE"
"$VENV/bin/python" -m pytest -p no:cacheprovider \
  --junitxml junit-xunit1.xml \
  -o junit_family=xunit1 \
  -o junit_suite_name=fastproof-samples
rc2=$?
set -e
echo "pytest exit codes: $rc1 $rc2"

cp "$GEN_DIR/environment.properties" "$GEN_DIR/categories.json" allure-results/

rm -rf "$OUT/allure-results" "$OUT/junit-xunit2.xml" "$OUT/junit-xunit1.xml"
mkdir -p "$OUT"
cp -R allure-results "$OUT/allure-results"
cp junit-xunit2.xml junit-xunit1.xml "$OUT/"

sanitize() {
  local f="$1"
  sed -i.bak \
    -e "s#$PROJ#$SANDBOX_PATH#g" \
    -e "s#$WORK#/builds/example#g" \
    -e "s#/home/$USER_NAME#/home/runner#g" \
    -e "s#$HOST#runner.example.com#g" \
    "$f"
  rm -f "$f.bak"
}

while IFS= read -r -d '' f; do
  case "$f" in
    *.png) ;;
    *) sanitize "$f" ;;
  esac
done < <(find "$OUT/allure-results" "$OUT/junit-xunit2.xml" "$OUT/junit-xunit1.xml" -type f -print0)

if grep -rIl -e '/tmp/' -e '/Users/' -e '/home/' -e "$HOST" "$OUT/allure-results" "$OUT/junit-xunit2.xml" "$OUT/junit-xunit1.xml"; then
  echo "sanitization left private paths or host names in the files above" >&2
  exit 1
fi

echo "outputs written to $OUT"
