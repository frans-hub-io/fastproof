import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, describe, it } from 'node:test';

import type { TestResult } from 'allure-js-commons';

import { FastProofAllure } from '../src/core';
import { FakeServer } from './fake-server';

const TOKEN = 'test-token-abcdef';
const SECRET = 'correct-horse-battery';

function setup(url: string) {
  const dir = mkdtempSync(join(tmpdir(), 'fp-allure-'));
  const resultsDir = join(dir, 'allure-results');
  mkdirSync(resultsDir);
  const lines: string[] = [];
  const fp = new FastProofAllure(
    {
      resultsDir,
      fastproof: { url, token: TOKEN, project: 'demo', spoolDir: join(dir, 'spool'), flushTimeoutMs: 5000, batchIntervalMs: 20, launch: 'ci-1' },
    },
    (m) => void lines.push(m),
  );
  return { fp, resultsDir, lines, dir };
}

function allureResult(uuid: string, extra: Partial<TestResult> = {}): TestResult {
  return {
    uuid,
    name: 'pay',
    fullName: 'checkout.spec.js › pay',
    testCaseId: 'tc-1',
    stage: 'pending' as TestResult['stage'],
    statusDetails: {},
    steps: [],
    attachments: [],
    parameters: [{ name: 'Project', value: 'chromium' }],
    labels: [{ name: 'framework', value: 'playwright' }, { name: '_fallbackTestCaseId', value: 'x' }],
    links: [],
    start: Date.now(),
    ...extra,
  } as TestResult;
}

describe('FastProofAllure listener', () => {
  const servers: FakeServer[] = [];
  after(async () => {
    for (const s of servers) await s.stop().catch(() => undefined);
  });

  it('opens a running result, then sends the final one with attempts, masking, files and traceparent', async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: true }).start();
    servers.push(srv);
    process.env.SHOP_PASSWORD = SECRET;
    const { fp, resultsDir } = setup(srv.url);
    delete process.env.SHOP_PASSWORD;
    fp.start();
    writeFileSync(join(resultsDir, 'a1-attachment.txt'), `body with ${SECRET}`);
    writeFileSync(join(resultsDir, 'a2-attachment.zip'), Buffer.from([80, 75, 3, 4]));
    const l = fp.listener;
    const first = allureResult('u1');
    l.afterTestResultStart!(first);
    assert.ok(first.labels.some((x) => x.name === 'traceparent' && x.value.startsWith('00-')));
    l.afterStepStop!({ name: 'inner', status: 'passed', start: 1, stop: 5 } as never);
    Object.assign(first, {
      status: 'failed',
      stage: 'finished',
      stop: Date.now() + 5,
      statusDetails: { message: `boom ${SECRET}`, trace: 'at x' },
      parameters: [{ name: 'Project', value: 'chromium' }, { name: 'token', value: SECRET, mode: 'masked' }],
      steps: [{ name: 'inner', status: 'passed', stage: 'finished', steps: [], attachments: [{ name: 'note', source: 'a1-attachment.txt', type: 'text/plain' }], parameters: [], statusDetails: {} }],
      attachments: [{ name: 'trace', source: 'a2-attachment.zip', type: 'application/vnd.allure.playwright-trace' }],
    });
    l.afterTestResultWrite!(first);
    const second = allureResult('u2');
    l.afterTestResultStart!(second);
    Object.assign(second, { status: 'passed', stage: 'finished', stop: Date.now() + 5 });
    l.afterTestResultWrite!(second);
    const s = await fp.close();
    assert.equal(s?.sent, 2);
    const ids = [...srv.results.keys()].sort();
    assert.equal(ids.length, 2);
    const r1 = srv.results.get(`aj-${ids[0]!.split('-')[1]}-u1`)!;
    const r2 = srv.results.get(`aj-${ids[0]!.split('-')[1]}-u2`)!;
    assert.deepEqual(r1.history, ['running', 'failed']);
    assert.equal(r1.body.retry, 0);
    assert.equal(r2.body.retry, 1);
    assert.notEqual((r1.body.traceparent as string).split('-')[1], (r2.body.traceparent as string).split('-')[1]);
    assert.equal(r1.body.testCaseId, 'tc-1');
    assert.ok(!JSON.stringify(srv.logs).includes(SECRET));
    assert.ok(!JSON.stringify([...srv.results.values()]).includes(SECRET));
    assert.deepEqual(
      (r1.body.parameters as { name: string }[]).map((p) => p.name),
      ['Project', 'token'],
    );
    assert.deepEqual((r1.body.labels as { name: string }[]).map((x) => x.name), ['framework', 'traceparent']);
    const names = (r1.body.attachments as { name: string }[]).map((a) => a.name).sort();
    assert.deepEqual(names, ['note.txt', 'trace.zip']);
    const note = [...srv.attachments.values()].find((a) => a.name === 'note.txt')!;
    assert.equal(note.body.toString(), 'body with ****');
    assert.ok(srv.logs.some((x) => x.stream === 'step' && String(x.message).includes('inner')));
    assert.equal([...srv.launches.values()][0]?.externalId, 'ci-1');
  });

  it('refuses attachment sources that leave the results directory', async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: true }).start();
    servers.push(srv);
    const { fp, dir } = setup(srv.url);
    fp.start();
    writeFileSync(join(dir, 'secret.txt'), 'outside');
    const r = allureResult('u9', { status: 'passed' as never, stop: Date.now() + 1, attachments: [{ name: 'x', source: '../secret.txt', type: 'text/plain' }] });
    fp.listener.afterTestResultStart!(r);
    fp.listener.afterTestResultWrite!(r);
    await fp.close();
    assert.equal(srv.attachments.size, 0);
    assert.equal(srv.results.size, 1);
  });

  it('closes unfinished attempts as broken', async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: true }).start();
    servers.push(srv);
    const { fp } = setup(srv.url);
    fp.start();
    fp.listener.afterTestResultStart!(allureResult('u5'));
    await fp.close('interrupted');
    const [r] = [...srv.results.values()];
    assert.equal(r?.status, 'broken');
    assert.deepEqual(r?.history, ['running', 'broken']);
  });

  it('stays off without a URL', async () => {
    const lines: string[] = [];
    const saved = { ...process.env };
    delete process.env.FASTPROOF_URL;
    delete process.env.FASTPROOF_PROJECT;
    const fp = new FastProofAllure({}, (m) => void lines.push(m));
    Object.assign(process.env, saved);
    assert.equal(fp.client, undefined);
    fp.listener.afterTestResultStart!(allureResult('u1'));
    assert.equal(await fp.close(), undefined);
    assert.match(lines.join('\n'), /FASTPROOF_URL is not set/);
  });
});
