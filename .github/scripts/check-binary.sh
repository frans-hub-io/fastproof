#!/usr/bin/env bash
set -euo pipefail

bin=${1:?usage: check-binary.sh <binary> <version> [build path...]}
expected=${2:?usage: check-binary.sh <binary> <version> [build path...]}
shift 2

out=$("$bin" version)
if [ "$out" != "fastproof $expected" ]; then
  echo "::error::$bin reports '$out', expected 'fastproof $expected'"
  exit 1
fi
echo "$bin reports '$out'"

paths=(/Users/ /home/runner/work/fastproof/ /home/runner/.cargo/ /home/runner/.rustup/)
for p in "$@"; do
  if [ -n "$p" ]; then
    paths+=("$p")
  fi
done
patterns=()
for p in "${paths[@]}"; do
  patterns+=(-e "$p")
done
found=$(strings -n 6 "$bin" | grep -F "${patterns[@]}" | head -n 20 || true)
if [ -n "$found" ]; then
  echo "::error::$bin contains local build paths"
  printf '%s\n' "$found"
  exit 1
fi
echo "$bin contains none of: ${paths[*]}"
