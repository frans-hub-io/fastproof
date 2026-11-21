# Vitest fixtures

Generated, not handwritten. Do not edit by hand; rerun the generator.

- Tools: Node 22, `vitest` 5.1.0, `vite` 8.3.6, `allure-vitest` 3.14.0, `allure-js-commons` 3.14.0, and
  `@vitest/runner` 4.1.12. Vitest 5 no longer ships `@vitest/runner`, but `allure-vitest` 3.14.0 still imports it, so
  the 4.1.12 package is pinned only to satisfy that import. Install uses `--legacy-peer-deps` (npm 10.9.8 crashes
  resolving the optional peers of `vite` 8).
- Command: `tests/fixtures/generators/vitest/generate.sh` (optional `WORK=<dir>` and `npm_config_cache=<dir>`).
- One run, `retry: 2`. Paths are rewritten to `/builds/example/fastproof-samples`, the host to `runner.example.com`.

| File | Contents |
| --- | --- |
| `allure-results/` | allure-vitest output: one `*-result.json` per test (not per attempt), attachments, `environment.properties`, `categories.json` |
| `junit.xml` | built-in `junit` reporter with `addFileAttribute: true` |

Covered cases:

- `Cart > adds item to cart`: passes; nested `allure.step`s with text, json and png (`attachmentPath`) attachments,
  a step parameter, epic/feature/story/severity/owner/tag/ALLURE_ID, issue/tms/plain links, description,
  parameters (plain, `masked`, `hidden`), a Vitest `annotate(..., "issue")`, stdout and stderr.
- `Cart > applies discount code`: `expect(95).toBe(90)` fails on all 3 attempts.
- `Cart > calculates tax`: TypeError, `{ retry: 0 }`.
- `Checkout > pays via gateway`: fails on the first attempt, passes on the second (module-level counter).
- `Checkout > formats <currency> with <n> decimals`: `test.each` (3 tests).
- `Checkout > bulk import` (`test.skip`, absent from allure-results), `legacy export` (`ctx.skip(reason)`),
  `refund flow` (`test.todo`).
- `Checkout > rounding bug`: `test.fails`, fails as expected.
