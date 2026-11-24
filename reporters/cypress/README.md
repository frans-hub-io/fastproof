# @franshub/fastproof-cypress

Cypress plugin for FastProof: every retry as an attempt with its own `traceparent`, secret masking, offline spool,
test plans, screenshots and video, and (with the support file) live `running` results and the command log. Built
on the core of `@franshub/fastproof-playwright` (a peer dependency). Wire contract:
[reporters/PROTOCOL.md](../PROTOCOL.md); user guide: the docs page "Reporters".

```js
// cypress.config.js
const { defineConfig } = require('cypress');
const { fastproof } = require('@franshub/fastproof-cypress');

module.exports = defineConfig({
  retries: { runMode: 2 },
  e2e: {
    setupNodeEvents(on, config) {
      return fastproof(on, config, { environment: 'staging', video: 'on-failure' });
    },
  },
});
```

```js
// cypress/support/e2e.js
import '@franshub/fastproof-cypress/support';
```

```sh
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_PROJECT=shop FASTPROOF_TOKEN=... npx cypress run
```

How it works. The support file (browser) calls `cy.task('fastproof:begin')` before and `fastproof:end` after each
attempt; the plugin (node) opens a `running` result, answers with the attempt's `traceparent` and keeps the command
log and the error. When a spec ends (`after:spec`) the plugin builds the final result of every attempt from
Cypress's result, adds what the browser told it, attaches the screenshots taken during that attempt and, for failed
attempts, the spec video (`video: 'always' | 'off'` change it), and sends it. Finals wait for the end of the spec
because the failure screenshot and the video exist only then. Without the support file everything except the
`running` results, the command log and the `traceparent` inside the test still works.

- `import { fastproofTrace } from '@franshub/fastproof-cypress/support'`: `fastproofTrace().headers()` for `cy.request`.
- Ids are written in the title: `[fastproof-id:ID]` or `@allure.id=ID`.
- Test plan (`FASTPROOF_TESTPLAN`): `specPattern` is narrowed to the files the plan names (when every entry has one),
  and the support file skips other tests (the plugin does not report them).
- Not supported: `cypress open`. Parallel `cypress run` processes with the same launch id share one launch (not tested).
- Offline spool: `npx fastproof-playwright flush --spool .fastproof/spool`.

Status of verification: the plugin and the support file are tested against a fake server and a real FastProof
server by driving them with the events Cypress emits, and typed against Cypress 16.2.1. The Cypress application
itself did not start in the build VM (Electron without a working display stack), so no run in real Cypress was made.

Development: `npm install --legacy-peer-deps && npm run build && npm test`;
`reporters/integration/test_cypress.py` replays an event sequence against a real FastProof server.
