# @franshub/fastproof-allure-js

Streams Allure JavaScript results to FastProof while the tests run. `allure-playwright` keeps writing
`allure-results`; this package adds a lifecycle listener (running results, live steps, attempts, `traceparent`,
masking, offline spool). Settings and behaviour: [reporters/PROTOCOL.md](../PROTOCOL.md) and the docs page
"Reporters". Built on the core of `@franshub/fastproof-playwright` (a peer dependency).

```ts
reporter: [['@franshub/fastproof-allure-js/playwright', { resultsDir: 'allure-results', fastproof: {} }]]
```

Other `allure-js-commons` adapters: `createFastProofListener()`, pass `.listener` in `listeners`, call `.start()` and
`await .close()` (verified only with allure-playwright 3.15.0 and `@playwright/test` 1.65.0).

Development: `npm install && npm run build && npm test`. The e2e test runs a real Playwright run against a fake
server; `reporters/integration/test_allure_js.py` runs it against a real FastProof server.
