# Playwright trace fixture

Generated, not handwritten: `tests/fixtures/generators/playwright-trace/generate.sh` (Node 22, `@playwright/test`
1.65.0, Chromium). One test with `trace: 'on'` types into a password field with `fill`, `pressSequentially` and
`press`. The files are the `*.trace` entries of its `trace.zip`: `test.trace` is written by the test runner (its
`pw:api` events carry the typed values in `title` and `params`), `0-trace.trace` and `1-trace.trace` by the
library (`Frame.fill`, `Frame.type`, `Frame.press` with the values in `params`). Paths are rewritten to
`/builds/example/fastproof-samples`. The `trace_actions` MCP tool must never return `hunter2-SECRET` or
`typed-SECRET2` from them.
