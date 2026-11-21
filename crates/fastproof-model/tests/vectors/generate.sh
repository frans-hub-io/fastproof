#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="${WORK:-$(mktemp -d)}"
curl -sL -o "$work/allure3.tgz" https://codeload.github.com/allure-framework/allure3/tar.gz/refs/tags/v3.33.0
tar xzf "$work/allure3.tgz" -C "$work"
src="$work/allure3-3.33.0/packages"
sed 's#^import { DEFAULT_ENVIRONMENT } from "./environment.js";#const DEFAULT_ENVIRONMENT = "default";#' "$src/core-api/src/utils/identity.ts" > "$work/identity.ts"
sed -e 's#^import type .*##' "$src/core-api/src/utils/flaky.ts" > "$work/flaky.ts"
sed -e 's#^import type .*##' "$src/core/src/utils/new.ts" > "$work/new.ts"
cp "$here/allure3-vectors.ts" "$work/vectors.ts"
(cd "$work" && node --experimental-strip-types --no-warnings vectors.ts) > "$work/out.json"
python3 -c "import json,sys; json.dump(json.load(open(sys.argv[1])), open(sys.argv[2],'w'), ensure_ascii=False, indent=1)" "$work/out.json" "$here/allure3-vectors.json"
