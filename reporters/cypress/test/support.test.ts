import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

describe('support file (fake Cypress globals)', () => {
  it('sends begin and end tasks with the command log and applies skip', async () => {
    const calls: { name: string; arg: any }[] = [];
    const hooks: { before: ((this: any) => void)[]; after: ((this: any) => void)[] } = { before: [], after: [] };
    const events = new Map<string, (...a: any[]) => void>();
    const g = globalThis as any;
    g.Cypress = {
      spec: { relative: 'cypress/e2e/a.cy.js' },
      currentTest: { titlePath: ['suite', 'test'] },
      currentRetry: 1,
      on: (e: string, h: (...a: any[]) => void) => events.set(e, h),
    };
    g.cy = {
      task: (name: string, arg: unknown) => {
        calls.push({ name, arg });
        return {
          then: (f: (r: unknown) => void) =>
            f(name === 'fastproof:begin' ? { skip: true, traceparent: `00-${'a'.repeat(32)}-${'b'.repeat(16)}-01`, traceId: 'a'.repeat(32), spanId: 'b'.repeat(16), baggage: 'k=v' } : null),
        };
      },
    };
    g.beforeEach = (f: () => void) => hooks.before.push(f);
    g.afterEach = (f: () => void) => hooks.after.push(f);
    const mod = require('../src/support') as typeof import('../src/support');
    assert.equal(hooks.before.length, 1);
    let skipped = false;
    hooks.before[0]!.call({ skip: () => (skipped = true) });
    assert.equal(skipped, true);
    assert.deepEqual(calls[0]!.arg.titlePath, ['suite', 'test']);
    assert.equal(calls[0]!.arg.retry, 1);
    events.get('command:start')!({ attributes: { name: 'visit', args: ['/cart'] } });
    events.get('command:start')!({ attributes: { name: 'task', args: ['x'] } });
    hooks.after[0]!.call({ currentTest: { state: 'failed', err: { name: 'AssertionError', message: 'm', stack: 's' } } });
    const end = calls[1]!;
    assert.equal(end.name, 'fastproof:end');
    assert.equal(end.arg.state, 'failed');
    assert.deepEqual(end.arg.logs.map((l: { m: string }) => l.m), ['visit /cart']);
    assert.equal(mod.fastproofTrace().headers().traceparent, `00-${'a'.repeat(32)}-${'b'.repeat(16)}-01`);
    assert.match(mod.fastproofTrace().headers({ newSpan: true }).traceparent!, /^00-a{32}-[0-9a-f]{16}-01$/);
    delete g.Cypress;
    delete g.cy;
    delete g.beforeEach;
    delete g.afterEach;
  });
});
