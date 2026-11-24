# @franshub/fastproof-vitest

Vitest reporter for FastProof: live results, console output, every retry and repeat as an attempt with its own
`traceparent`, secret masking, offline spool, test plans and attachments (large ones directly to S3 when the server
offers it). Built on the core of `@franshub/fastproof-playwright` (a peer dependency). Wire contract:
[reporters/PROTOCOL.md](../PROTOCOL.md); user guide: the docs page "Reporters".

```ts
// vitest.config.ts
import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    retry: 2,
    reporters: ['default', ['@franshub/fastproof-vitest', { environment: 'staging' }]],
    setupFiles: ['@franshub/fastproof-vitest/setup'],
  },
});
```

```sh
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_PROJECT=shop FASTPROOF_TOKEN=... npx vitest run
```

- Options are those of the Playwright reporter (`url`, `project`, `launch`, `environment`, `secrets`, `spoolDir`,
  `directUploads`, ...) plus `testPlan`; the environment variables of PROTOCOL.md section 1 work too.
- Test case ids and labels: `test('pays', { meta: { fastproofId: 'CASE-7', fastproofLabels: { owner: 'team-a' },
  fastproofLinks: ['https://tracker.example.com/T-1'] } }, ...)`.
- Trace: `import { fastproofTrace } from '@franshub/fastproof-vitest/trace'` then `fastproofTrace(task).headers()`
  in a test (`task` comes from the test context), or `.env()` for child processes.
- Attachments: `annotate(name, type, { body | path, contentType })` and browser-mode artifacts, uploaded with the
  last attempt (Vitest does not say which attempt called `annotate`).
- Test plan (`FASTPROOF_TESTPLAN`): the `setup` file skips tests outside the plan, the reporter does not report
  them. `file:line` selectors need `includeTaskLocation: true`.
- Vitest sends task events to the reporter in batches: a result is `running` shortly after the test starts.
  Console output of tests without retries streams live; with retries it is assigned to the attempt by time
  once the test ends. Vitest 4 does not list attempts, so there earlier attempts carry the status `failed` and
  the error of that attempt without timing.
- Not supported: watch mode (the reporter does nothing), Vitest below 4.1.
- Offline spool: `npx fastproof-playwright flush --spool .fastproof/spool` (same format as the other reporters).

Development: `npm install --legacy-peer-deps && npm run build && npm test` (needs `../playwright` built). The e2e
test runs real Vitest against a fake server; `reporters/integration/test_vitest.py` runs it against a real
FastProof server.
