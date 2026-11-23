import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { after, describe, it } from 'node:test';

import { FakeServer } from './fake-server';

const ROOT = resolve(__dirname, '..', '..');
const SECRET = 'pw-secret-xyz';
const TOKEN = 'test-token-abcdef';

describe('allure-playwright with FastProof attached (real Playwright run, fake server)', () => {
  const servers: FakeServer[] = [];
  after(async () => {
    for (const s of servers) await s.stop().catch(() => undefined);
  });

  it('streams attempts, steps, attachments and traceparent while allure-results are written', { skip: !existsSync(join(ROOT, 'dist', 'playwright.js')) }, async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: true }).start();
    servers.push(srv);
    const dir = mkdtempSync(join(tmpdir(), 'fp-allure-e2e-'));
    const env = {
      ...process.env,
      FASTPROOF_URL: srv.url,
      FASTPROOF_TOKEN: TOKEN,
      FASTPROOF_PROJECT: 'demo',
      FASTPROOF_LAUNCH: 'ci-e2e',
      E2E_SECRET_TOKEN: SECRET,
      E2E_OUTPUT_DIR: join(dir, 'out'),
      E2E_RESULTS_DIR: join(dir, 'allure-results'),
      FASTPROOF_SPOOL_DIR: join(dir, 'spool'),
    };
    const p = spawn('npx', ['playwright', 'test', '-c', 'e2e'], { cwd: ROOT, env });
    let output = '';
    p.stdout.on('data', (d: Buffer) => (output += d));
    p.stderr.on('data', (d: Buffer) => (output += d));
    const code = await new Promise<number | null>((r) => p.on('close', r));
    assert.equal(code, 1, output);
    assert.match(output, /\[fastproof\] launch #1: 7 results/);
    const results = [...srv.results.values()];
    assert.equal(results.length, 7);
    for (const r of results) assert.equal(r.history[0], 'running', r.id);
    const byName = new Map<string, typeof results>();
    for (const r of results) byName.set(String(r.body.name), [...(byName.get(String(r.body.name)) ?? []), r]);
    const flaky = byName.get('flaky: passes on retry')!.sort((a, b) => Number(a.body.retry) - Number(b.body.retry));
    assert.deepEqual(flaky.map((r) => r.status), ['failed', 'passed']);
    assert.deepEqual(flaky.map((r) => r.body.retry), [0, 1]);
    assert.equal(byName.get('fails every time')!.length, 2);
    assert.equal(byName.get('skipped')![0]!.status, 'skipped');
    const dump = JSON.stringify([...srv.results.values()]) + JSON.stringify(srv.logs) + JSON.stringify([...srv.attachments.values()].map((a) => a.body.toString('latin1')));
    assert.ok(!dump.includes(SECRET));
    const note = [...srv.attachments.values()].find((a) => a.name.startsWith('note-'))!;
    assert.equal(note.body.toString(), 'body with ****');
    assert.ok(srv.logs.some((l) => l.stream === 'step' && String(l.message).includes('inner step')));
    const trace = byName.get('sends traceparent to the backend')![0]!;
    const seen = JSON.parse([...srv.attachments.values()].find((a) => a.name === 'seen-headers.json')!.body.toString()) as { traceparent: string };
    assert.equal(seen.traceparent, trace.body.traceparent);
    const written = readdirSync(join(dir, 'allure-results')).filter((f) => f.endsWith('-result.json'));
    assert.equal(written.length, 7);
    const labelled = written.map((f) => JSON.parse(readFileSync(join(dir, 'allure-results', f), 'utf8')) as { labels: { name: string; value: string }[] });
    const tps = new Set(labelled.map((w) => w.labels.find((l) => l.name === 'traceparent')?.value));
    assert.equal(tps.size, 7);
    assert.ok(results.every((r) => tps.has(r.body.traceparent as string)));
    assert.equal([...srv.launches.values()][0]?.finished, undefined);
  });
});
