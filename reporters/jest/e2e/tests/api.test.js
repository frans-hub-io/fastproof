const fs = require('node:fs');
const path = require('node:path');
const { attach, step, fastproofTrace } = require('../../dist/setup.js');

const state = process.env.E2E_STATE_DIR || '.';

function attempt(name) {
  const f = path.join(state, `${name}.count`);
  const n = fs.existsSync(f) ? Number(fs.readFileSync(f, 'utf8')) : 0;
  fs.writeFileSync(f, String(n + 1));
  return n;
}

jest.retryTimes(2);

describe('checkout', () => {
  test('passes with output [fastproof-id:CASE-1]', async () => {
    console.log('starting with token', process.env.E2E_SECRET_TOKEN);
    console.error('a warning');
    await step('outer step', async () => {
      await step('inner step', async () => {});
    });
    expect(1 + 1).toBe(2);
  });

  test('flaky: passes on third try', () => {
    const n = attempt('flaky');
    console.log(`flaky attempt ${n}`);
    attach('note', `body with ${process.env.E2E_SECRET_TOKEN}`, 'text/plain');
    expect(n).toBeGreaterThanOrEqual(2);
  });

  test('fails every time', () => {
    expect('a').toBe('b');
  });

  test('throws a plain error', () => {
    throw new TypeError('not an assertion');
  });

  test.skip('skipped', () => {});

  test.todo('later');

  test.each([[1], [2]])('param %i', (n) => {
    expect(n).toBeGreaterThan(0);
  });

  test('sends traceparent', () => {
    attach('seen-headers.json', JSON.stringify(fastproofTrace().headers()), 'application/json');
  });

  test('big attachment', () => {
    attach('big.bin', Buffer.alloc(2 * 1024 * 1024, 7));
  });
});
