import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, test } from 'vitest';

import { fastproofTrace } from '../../dist/trace.js';

const state = process.env.E2E_STATE_DIR ?? '.';

function attempt(name: string): number {
  const f = join(state, `${name}.count`);
  const n = existsSync(f) ? Number(readFileSync(f, 'utf8')) : 0;
  writeFileSync(f, String(n + 1));
  return n;
}

describe('checkout', () => {
  test('passes with output', { meta: { fastproofId: 'CASE-1', fastproofLabels: { owner: 'team-a' } } }, () => {
    console.log('starting with token', process.env.E2E_SECRET_TOKEN);
    console.error('a warning');
    expect(1 + 1).toBe(2);
  });

  test('flaky: passes on third try', { retry: 2 }, async ({ annotate }) => {
    const n = attempt('flaky');
    console.log(`flaky attempt ${n}`);
    await annotate('note', 'info', { body: `body with ${process.env.E2E_SECRET_TOKEN}`, contentType: 'text/plain', bodyEncoding: 'utf-8' });
    expect(n).toBeGreaterThanOrEqual(2);
  });

  test('fails every time', { retry: 1 }, () => {
    expect('a').toBe('b');
  });

  test('throws a plain error', () => {
    throw new TypeError('not an assertion');
  });

  test.skip('skipped', () => {});

  test.each([[1], [2]])('param %i', (n) => {
    expect(n).toBeGreaterThan(0);
  });

  test('sends traceparent', async ({ task, annotate }) => {
    const h = fastproofTrace(task).headers();
    await annotate('seen-headers.json', 'info', { body: JSON.stringify(h), contentType: 'application/json', bodyEncoding: 'utf-8' });
  });

  test('big attachment', async ({ annotate }) => {
    await annotate('big.bin', 'info', { body: Buffer.alloc(2 * 1024 * 1024, 7), contentType: 'application/octet-stream' });
  });
});
