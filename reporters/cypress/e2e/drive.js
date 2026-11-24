const { mkdtempSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');
const { CypressFastProof } = require('../dist/plugin.js');

async function main() {
  const handlers = new Map();
  let tasks = {};
  const plugin = new CypressFastProof({ flushTimeoutMs: 15000 });
  plugin.register((event, h) => (event === 'task' ? (tasks = h) : handlers.set(event, h)), {});
  const fire = (event, ...args) => handlers.get(event)(...args);
  const dir = mkdtempSync(join(tmpdir(), 'fp-cy-drive-'));
  const shot = join(dir, 'failed.png');
  writeFileSync(shot, Buffer.alloc(100, 1));
  const video = join(dir, 'cart.cy.js.mp4');
  writeFileSync(video, Buffer.alloc(2 * 1024 * 1024, 2));
  const spec = { relative: 'cypress/e2e/cart.cy.js', name: 'cart.cy.js' };
  const secret = process.env.E2E_SECRET_TOKEN;
  const base = Date.now() - 60000;
  await fire('before:run', {});
  const flaky = ['cart', 'flaky'];
  const b0 = tasks['fastproof:begin']({ spec: spec.relative, titlePath: flaky, retry: 0, start: base });
  tasks['fastproof:end']({
    spec: spec.relative, titlePath: flaky, retry: 0, state: 'failed',
    err: { name: 'AssertionError', message: `expected ${secret} to equal 1`, stack: 'AssertionError' },
    logs: [{ t: base + 500, m: `visit /cart ${secret}` }], stop: base + 2000,
  });
  await fire('after:screenshot', { path: shot, name: 'cart -- flaky (failed)', takenAt: new Date(base + 2000).toISOString(), specName: spec.name });
  const b1 = tasks['fastproof:begin']({ spec: spec.relative, titlePath: flaky, retry: 1, start: base + 3000 });
  tasks['fastproof:end']({ spec: spec.relative, titlePath: flaky, retry: 1, state: 'passed', logs: [{ t: base + 3500, m: 'get .cart' }], stop: base + 4000 });
  await fire('after:spec', spec, {
    video,
    stats: { startedAt: new Date(base).toISOString() },
    tests: [
      {
        title: flaky, state: 'passed',
        attempts: [
          { state: 'failed', error: { name: 'AssertionError', message: `expected ${secret} to equal 1`, stack: 'AssertionError' }, duration: 2000, startedAt: new Date(base).toISOString() },
          { state: 'passed', error: null, duration: 1000, startedAt: new Date(base + 3000).toISOString() },
        ],
      },
      { title: ['cart', 'broken one'], state: 'failed', displayError: 'TypeError: x', attempts: [{ state: 'failed', error: { name: 'TypeError', message: 'x is not a function' }, duration: 10 }] },
      { title: ['cart', 'later'], state: 'pending', attempts: [{ state: 'pending' }] },
    ],
  });
  await fire('after:run', { status: 'finished' });
  process.stdout.write(JSON.stringify({ first: b0.traceId, second: b1.traceId }) + '\n');
}

main().catch((e) => {
  process.stderr.write(String(e) + '\n');
  process.exit(1);
});
