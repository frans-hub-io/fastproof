#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../../.." && pwd)"
OUT="$REPO/tests/fixtures/junit5"
WORK="${WORK:-$(mktemp -d)}"
WORK="$(cd "$WORK" && pwd -P)"
MVN="${MVN:-mvn}"
MAVEN_REPO="${MAVEN_REPO:-$WORK/m2-repository}"
PROJ="$WORK/fastproof-samples"

MVN_BIN="$(realpath "$(command -v "$MVN")")"
MVN_HOME="$(dirname "$(dirname "$MVN_BIN")")"
JDK_HOME="${JAVA_HOME:-$(dirname "$(dirname "$(realpath "$(command -v java)")")")}"
JDK_HOME="$(realpath "$JDK_HOME")"
mkdir -p "$MAVEN_REPO"
MAVEN_REPO="$(realpath "$MAVEN_REPO")"

rm -rf "$PROJ"
mkdir -p "$PROJ"
cp "$HERE/pom.xml" "$PROJ/"
cp -R "$HERE/src" "$PROJ/"

set +e
(cd "$PROJ" && "$MVN" -B -ntp -Dmaven.repo.local="$MAVEN_REPO" test) > "$WORK/maven.log" 2>&1
STATUS=$?
set -e
echo "maven exit code: $STATUS, log: $WORK/maven.log"

if [ ! -d "$PROJ/target/allure-results" ] || ! ls "$PROJ"/target/surefire-reports/TEST-*.xml > /dev/null 2>&1; then
  tail -n 50 "$WORK/maven.log"
  exit 1
fi

rm -rf "$OUT/allure-results" "$OUT/surefire-reports"
mkdir -p "$OUT/allure-results" "$OUT/surefire-reports"
cp "$PROJ"/target/allure-results/* "$OUT/allure-results/"
cp "$PROJ"/target/surefire-reports/TEST-*.xml "$OUT/surefire-reports/"

export SAN_PROJ="$PROJ" SAN_REPO="$MAVEN_REPO" SAN_WORK="$WORK" SAN_JDK="$JDK_HOME" SAN_MVN="$MVN_HOME"
export SAN_HOMEDIR="$HOME" SAN_HOST="$(hostname)" SAN_USER="$(id -un)" SAN_TMP="${TMPDIR:-/tmp}"

find "$OUT/allure-results" "$OUT/surefire-reports" -type f \( -name '*.json' -o -name '*.txt' -o -name '*.xml' -o -name '*.httpexchange' \) -print0 |
  xargs -0 perl -pi -e '
    s/\Q$ENV{SAN_PROJ}\E/\/builds\/example\/fastproof-samples/g;
    s/\Q$ENV{SAN_REPO}\E/\/builds\/example\/.m2\/repository/g;
    s/\Q$ENV{SAN_WORK}\E/\/builds\/example/g;
    s/\Q$ENV{SAN_JDK}\E/\/opt\/java\/openjdk/g;
    s/\Q$ENV{SAN_MVN}\E/\/opt\/maven/g;
    s/\Q$ENV{SAN_HOMEDIR}\E/\/root/g;
    s/\b\Q$ENV{SAN_HOST}\E\b/runner.example.com/g;
    s/(name="user\.name" value=")\Q$ENV{SAN_USER}\E"/$1runner"/g;
    s/(name="java\.io\.tmpdir" value=")\Q$ENV{SAN_TMP}\E"/$1\/tmp"/g;
    s/(name="(?:env\.)?[A-Za-z_.]+" value=")[^"]*\b\Q$ENV{SAN_USER}\E\b[^"]*"/$1\/redacted"/g;
  '

LEAKS="$(grep -rIlE "/tmp/|/Users/|/home/|$(id -un)@|$(hostname)" "$OUT/allure-results" "$OUT/surefire-reports" || true)"
if [ -n "$LEAKS" ]; then
  echo "unsanitized values remain in:" >&2
  echo "$LEAKS" >&2
  exit 1
fi

echo "allure-results: $(ls "$OUT/allure-results" | wc -l) files"
echo "surefire-reports: $(ls "$OUT/surefire-reports" | wc -l) files"
du -sh "$OUT"
