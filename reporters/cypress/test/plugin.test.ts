import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, describe, it } from 'node:test';

import { CypressFastProof } from '../src/plugin';
import { FakeServer } from './fake-server';

const SECRET = 'cy-secret-xyz-4242';
const TOKEN = 'test-token-abcdef';

type Handler = (...a: any[]) => any;

class Cy {
  handlers = new Map<string, Handler>();
  tasks: Record<string, Handler> = {};
  plugin: CypressFastProof;
  config: Record<string, unknown> = {};
  constructor(srv: FakeServer, dir: string, extra: Record<string, unknown> = {}) {
    process.env.E2E_SECRET_TOKEN = SECRET;
    this.plugin = new CypressFastProof({
      url: srv.url,
      token: TOKEN,
      project: 'demo',
      launch: 'ci-cypress',
      spoolDir: join(dir, 'spool'),
      flushTimeoutMs: 10_000,
      ...extra,
    });
    this.plugin.register(
      (event: string, h: any) => {
        if (event === 'task') this.tasks = h;
        else this.handlers.set(event, h);
      },
      this.config,
    );
  }
  async fire(event: string, ...args: any[]): Promise<void> {
    await this.handlers.get(event)?.(...args);
  }
}

const SPEC = { relative: 'cypress/e2e/cart.cy.js', name: 'cart.cy.js', absolute: '/p/cypress/e2e/cart.cy.js' };

describe('cypress plugin (events driven as Cypress does, fake server)', () => {
  const servers: FakeServer[] = [];
  after(async () => {
    for (const s of servers) await s.stop().catch(() => undefined);
  });

  it('reports attempts, live logs, screenshots and video with the support file', async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: true, direct: true }).start();
    servers.push(srv);
    const dir = mkdtempSync(join(tmpdir(), 'fp-cypress-'));
    const shot = join(dir, 'fail.png');
    writeFileSync(shot, Buffer.alloc(100, 1));
    const video = join(dir, 'cart.cy.js.mp4');
    writeFileSync(video, Buffer.alloc(2 * 1024 * 1024, 2));
    const cy = new Cy(srv, dir);
    await cy.fire('before:run', {});
    const titlePath = ['cart', 'flaky [fastproof-id:CASE-9]'];
    const b0 = cy.tasks['fastproof:begin']!({ spec: SPEC.relative, titlePath, retry: 0, start: Date.parse('2026-11-24T10:00:00Z') });
    assert.equal(b0.skip, false);
    cy.tasks['fastproof:end']!({
      spec: SPEC.relative,
      titlePath,
      retry: 0,
      state: 'failed',
      err: { name: 'AssertionError', message: `expected ${SECRET} to equal 1`, stack: 'stack' },
      logs: [{ t: Date.parse('2026-11-24T10:00:01Z'), m: `visit /cart ${SECRET}` }],
      stop: Date.parse('2026-11-24T10:00:02Z'),
    });
    await cy.fire('after:screenshot', { path: shot, name: 'cart -- flaky (failed)', takenAt: '2026-11-24T10:00:02Z', specName: 'cart.cy.js' });
    const b1 = cy.tasks['fastproof:begin']!({ spec: SPEC.relative, titlePath, retry: 1, start: Date.parse('2026-11-24T10:00:03Z') });
    assert.notEqual(b0.traceId, b1.traceId);
    cy.tasks['fastproof:end']!({ spec: SPEC.relative, titlePath, retry: 1, state: 'passed', logs: [{ t: 1, m: 'get .cart' }], stop: Date.parse('2026-11-24T10:00:04Z') });
    cy.tasks['fastproof:begin']!({ spec: SPEC.relative, titlePath: ['cart', 'ordinary'], retry: 0, start: Date.parse('2026-11-24T10:00:05Z') });
    await cy.fire('after:spec', SPEC, {
      video,
      stats: { startedAt: '2026-11-24T10:00:00Z' },
      tests: [
        {
          title: titlePath,
          state: 'passed',
          displayError: null,
          attempts: [
            { state: 'failed', error: { name: 'AssertionError', message: `expected ${SECRET} to equal 1`, stack: 'stack' }, duration: 2000, startedAt: '2026-11-24T10:00:00Z' },
            { state: 'passed', error: null, duration: 1000, startedAt: '2026-11-24T10:00:03Z' },
          ],
        },
        { title: ['cart', 'ordinary'], state: 'passed', attempts: [{ state: 'passed', duration: 500, startedAt: '2026-11-24T10:00:05Z' }] },
        { title: ['cart', 'broken one'], state: 'failed', displayError: 'TypeError: x', attempts: [{ state: 'failed', error: { name: 'TypeError', message: 'x is not a function' }, duration: 10 }] },
        { title: ['cart', 'later'], state: 'pending', attempts: [{ state: 'pending' }] },
      ],
      screenshots: [],
    });
    await cy.fire('after:run', { status: 'finished' });
    const results = [...srv.results.values()];
    const flaky = results.filter((r) => r.body.name === 'flaky [fastproof-id:CASE-9]').sort((a, b) => Number(a.body.retry) - Number(b.body.retry));
    assert.deepEqual(flaky.map((r) => r.status), ['failed', 'passed']);
    assert.equal(flaky[0]!.history[0], 'running');
    assert.equal(flaky[0]!.body.testCaseId, 'CASE-9');
    assert.equal(String(flaky[0]!.body.traceparent).split('-')[1], b0.traceId);
    assert.equal(String(flaky[1]!.body.traceparent).split('-')[1], b1.traceId);
    assert.equal(results.find((r) => r.body.name === 'broken one')!.status, 'broken');
    assert.equal(results.find((r) => r.body.name === 'later')!.status, 'skipped');
    assert.ok(srv.logs.some((l) => String(l.message) === 'visit /cart ****' && l.attempt === 0));
    assert.ok(srv.logs.some((l) => String(l.message) === 'get .cart' && l.attempt === 1));
    const atts = [...srv.attachments.values()];
    assert.ok(atts.some((a) => a.name === 'cart -- flaky (failed)'));
    assert.equal(atts.filter((a) => a.name === 'video.mp4').length, 1);
    assert.equal(srv.directPuts, 1);
    const dump = JSON.stringify(results) + JSON.stringify(srv.logs);
    assert.ok(!dump.includes(SECRET));
    assert.equal((flaky[0]!.body.attachments as unknown[]).length, 2);
    assert.ok(!(flaky[1]!.body.attachments as unknown[] | undefined)?.length);
  });

  it('builds results from the spec result alone when the support file is not used', async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: false }).start();
    servers.push(srv);
    const dir = mkdtempSync(join(tmpdir(), 'fp-cypress-'));
    const cy = new Cy(srv, dir);
    await cy.fire('before:run', {});
    await cy.fire('after:spec', SPEC, {
      tests: [
        { title: ['a', 'one'], state: 'failed', displayError: 'boom', attempts: [{ state: 'failed', error: { name: 'Error', message: 'boom' }, duration: 5 }, { state: 'failed', error: { name: 'Error', message: 'boom' }, duration: 5 }] },
      ],
    });
    await cy.fire('after:run', { status: 'finished' });
    const results = [...srv.results.values()];
    assert.equal(results.length, 2);
    assert.deepEqual(results.map((r) => r.body.retry).sort(), [0, 1]);
    assert.ok(results.every((r) => String(r.body.traceparent).startsWith('00-')));
  });

  it('skips tests outside the test plan and narrows the spec pattern', async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: true }).start();
    servers.push(srv);
    const dir = mkdtempSync(join(tmpdir(), 'fp-cypress-'));
    const plan = join(dir, 'testplan.json');
    writeFileSync(plan, JSON.stringify({ version: '1.0', tests: [{ selector: 'cypress/e2e/cart.cy.js#cart one' }] }));
    const cy = new Cy(srv, dir, { testPlan: plan });
    assert.deepEqual(cy.config.specPattern, ['cypress/e2e/cart.cy.js']);
    await cy.fire('before:run', {});
    assert.equal(cy.tasks['fastproof:begin']!({ spec: SPEC.relative, titlePath: ['cart', 'one'], retry: 0, start: 1 }).skip, false);
    assert.equal(cy.tasks['fastproof:begin']!({ spec: SPEC.relative, titlePath: ['cart', 'two'], retry: 0, start: 1 }).skip, true);
    await cy.fire('after:spec', SPEC, {
      tests: [
        { title: ['cart', 'one'], state: 'passed', attempts: [{ state: 'passed', duration: 1 }] },
        { title: ['cart', 'two'], state: 'passed', attempts: [{ state: 'passed', duration: 1 }] },
      ],
    });
    await cy.fire('after:run', { status: 'finished' });
    assert.deepEqual([...srv.results.values()].map((r) => r.body.name), ['one']);
  });

  it('ignores malformed task payloads', async () => {
    const srv = await new FakeServer({ token: TOKEN }).start();
    servers.push(srv);
    const cy = new Cy(srv, mkdtempSync(join(tmpdir(), 'fp-cypress-')));
    await cy.fire('before:run', {});
    assert.equal(cy.tasks['fastproof:begin']!(null), null);
    assert.equal(cy.tasks['fastproof:end']!({ titlePath: 'x' }), null);
    assert.equal(cy.tasks['fastproof:begin']!({ titlePath: ['a'], retry: 'NaN', spec: 5 }).skip, false);
    await cy.fire('after:run', { status: 'finished' });
  });
});
