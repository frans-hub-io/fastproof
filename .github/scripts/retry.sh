#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: retry.sh <command> [args...]" >&2
  exit 2
fi
attempts=${RETRY_ATTEMPTS:-5}
delay=${RETRY_FIRST_DELAY:-15}
for attempt in $(seq 1 "$attempts"); do
  if "$@"; then
    exit 0
  fi
  if [ "$attempt" -lt "$attempts" ]; then
    echo "::warning::'$*' failed (attempt $attempt of $attempts), retrying in ${delay}s"
    sleep "$delay"
    delay=$((delay * 2))
  fi
done
echo "::error::'$*' failed $attempts times"
exit 1
