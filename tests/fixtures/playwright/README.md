# Playwright fixtures

Generated, not handwritten. Do not edit by hand; rerun the generator.

- Tools: Node 22, `@playwright/test` 1.65.0, `allure-playwright` 3.14.0, `allure-js-commons` 3.14.0,
  `playwright-ctrf-json-reporter` 0.2.0 (CTRF specVersion 0.2.0). No browser is used.
- Command: `tests/fixtures/generators/playwright/generate.sh` (optional `WORK=<dir>` and `npm_config_cache=<dir>`).
- One run, `retries: 2`, two projects: `staging` (all tests) and `production` (only `@smoke`).
- Paths are rewritten to `/builds/example/fastproof-samples`, the host to `runner.example.com`.

| File | Contents |
| --- | --- |
| `allure-results/` | allure-playwright output: one `*-result.json` per attempt, attachments, `environment.properties`, `categories.json` |
| `results.json` | built-in `json` reporter |
| `junit.xml` | built-in `junit` reporter |
| `ctrf/ctrf-report.json` | `playwright-ctrf-json-reporter` |
| `test-results/` | Playwright output dir: `error-context.md` per failed attempt and the png attachment that the reporters reference by path |

Covered cases (project `staging` unless noted):

- `Cart › adds item to cart`: passes; nested `test.step`s with text and json attachments (`body`) and a png (`path`),
  an `allure.step` with a step parameter, Allure epic/feature/story/severity/owner/tag/ALLURE_ID, issue/tms/plain
  links, description, parameters (plain, `masked`, `hidden`), Playwright tags and annotations (`issue`, `note`),
  stdout and stderr.
- `Checkout › applies discount code`: `expect(95).toBe(90)` fails on all 3 attempts.
- `Checkout › Tax › calculates tax`: TypeError, retries disabled with `test.describe.configure({ retries: 0 })`.
- `Checkout › pays via gateway` (`@smoke`, both projects): fails on retry 0, passes on retry 1 (flaky).
- `Catalog › search for <query>`: parameterized by a for-loop (3 tests).
- `Catalog › legacy export` (`test.skip(true, reason)` at runtime), `bulk import` (`test.skip`), `price history chart`
  (`test.fixme`).
- `Catalog › rounding bug`: `test.fail()`, fails as expected.
