#!/usr/bin/env bash
set -euo pipefail

usage="usage: fetch.sh <sha256|sha512> <checksum> <output> <url>..."
algo=${1:?$usage}
sum=${2:?$usage}
out=${3:?$usage}
shift 3
if [ "$#" -eq 0 ]; then
  echo "$usage" >&2
  exit 2
fi
case "$algo" in
  sha256 | sha512) ;;
  *)
    echo "::error::unknown checksum algorithm $algo" >&2
    exit 2
    ;;
esac

verify() {
  echo "$sum  $1" | "${algo}sum" -c --status -
}

if [ -f "$out" ] && verify "$out"; then
  echo "$out is cached and matches its $algo checksum"
  exit 0
fi
rm -f "$out"
mkdir -p "$(dirname "$out")"
part="$out.part"
rounds=${FETCH_ROUNDS:-5}
delay=${FETCH_FIRST_DELAY:-15}
for round in $(seq 1 "$rounds"); do
  for url in "$@"; do
    rm -f "$part"
    if curl -fsSL --proto '=https' --connect-timeout 20 --max-time 600 \
      --retry 3 --retry-all-errors --retry-max-time 120 -o "$part" "$url"; then
      if verify "$part"; then
        mv "$part" "$out"
        echo "fetched $url"
        exit 0
      fi
      echo "::warning::$url does not match the expected $algo checksum"
    else
      echo "::warning::fetching $url failed (round $round of $rounds)"
    fi
  done
  if [ "$round" -lt "$rounds" ]; then
    echo "waiting ${delay}s before the next round"
    sleep "$delay"
    delay=$((delay * 2))
  fi
done
rm -f "$part"
echo "::error::could not fetch $out from any of: $*"
exit 1
