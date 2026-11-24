# @franshub/fastproof-jest

Jest reporter for FastProof: live results, console output, steps and attachments, every `jest.retryTimes` attempt
as an attempt with its own `traceparent`, secret masking, offline spool and test plans. Built on the core of
`@franshub/fastproof-playwright` (a peer dependency). Wire contract: [reporters/PROTOCOL.md](../PROTOCOL.md);
user guide: the docs page "Reporters".

```js
// jest.config.js
module.exports = {
  reporters: ['default', ['@franshub/fastproof-jest', { environment: 'staging' }]],
  setupFilesAfterEnv: ['@franshub/fastproof-jest/setup'],
};
```

```sh
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_PROJECT=shop FASTPROOF_TOKEN=... npx jest
```

How it works: the reporter (main process) gets `onTestCaseStart` / `onTestCaseResult` for every attempt, so it
sends a `running` result at the start and the final result at the end of each attempt. Jest does not report
console output, steps or attachments per test, so the `setup` file (in the test processes) writes them to a
private directory the reporter creates (`FASTPROOF_JEST_STAGE`), and the reporter reads it while the attempt runs.
Workers need no credentials and open no connections. Without the setup file results are sent without output.

```js
const { attach, attachFile, step, fastproofTrace } = require('@franshub/fastproof-jest/setup');

test('pays [fastproof-id:CASE-7]', async () => {
  console.log('paying');
  await step('call the API', async () => { await fetch(url, { headers: fastproofTrace().headers() }); });
  attach('response.json', JSON.stringify(body), 'application/json');
  attachFile('screenshots/pay.png', 'pay.png', 'image/png');
});
```

- `attach` / `attachFile` upload when the attempt ends (large files directly to S3 when the server offers it).
- Ids are written in the title: `[fastproof-id:ID]` or `@allure.id=ID`. Statuses: assertion failures are
  `failed`, other errors `broken`, skipped, todo and pending tests `skipped`; a file that fails to load is a
  broken result `<file> (suite failed to run)`.
- Test plan (`FASTPROOF_TESTPLAN`): `testPlanPattern()` gives a `testNamePattern` for the config; the reporter
  leaves out of the launch any test the plan does not select. Jest reports no locations, so `file:line`
  selectors do not match.
- Not supported: watch mode, `test.concurrent` (output may land on another test), the same title twice in one file.
- Offline spool: `npx fastproof-playwright flush --spool .fastproof/spool`.

Development: `npm install --legacy-peer-deps && npm run build && npm test`. The e2e test runs real Jest against a
fake server; `reporters/integration/test_jest.py` runs it against a real FastProof server.
