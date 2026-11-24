#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
out="$(cd "$here/../.." && pwd)/allure-history"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir "$work/a2" "$work/a3"
cp "$here/package-allure2.json" "$work/a2/package.json"
cp "$here/package-allure3.json" "$work/a3/package.json"
(cd "$work/a2" && npm install --no-audit --no-fund)
(cd "$work/a3" && npm install --no-audit --no-fund)
cp "$here/allurerc.mjs" "$here/make-results.py" "$work/"
cd "$work"
for b in 1 2 3; do
  python3 make-results.py "$b" "results$b"
  if [ "$b" -gt 1 ]; then cp -r "report$((b - 1))/history" "results$b/history"; fi
  ./a2/node_modules/.bin/allure generate "results$b" -o "report$b" --clean
  ./a3/node_modules/.bin/allure generate "results$b" --config allurerc.mjs --cwd . --clean
  sleep 1
done
rm -rf "$out/allure2" "$out/allure3" "$out/allure-results"
mkdir -p "$out/allure2/history" "$out/allure3" "$out/allure-results"
cp report3/history/*.json "$out/allure2/history/"
cp history.jsonl "$out/allure3/history.jsonl"
cp results3/*-result.json "$out/allure-results/"
