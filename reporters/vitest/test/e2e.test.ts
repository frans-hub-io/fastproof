import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { after, describe, it } from 'node:test';
import { fileURLToPath } from 'node:url';

import { FakeServer } from './fake-server.js';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const SECRET = 'vt-secret-xyz-4242';
const TOKEN = 'test-token-abcdef';

async function run(srv: FakeServer, extra: Record<string, string> = {}): Promise<{ code: number | null; output: string; dir: string }> {
  const dir = mkdtempSync(join(tmpdir(), 'fp-vitest-e2e-'));
  mkdirSync(join(dir, 'state'));
  const env = {
    ...process.env,
    FASTPROOF_URL: srv.url,
    FASTPROOF_TOKEN: TOKEN,
    FASTPROOF_PROJECT: 'demo',
    FASTPROOF_LAUNCH: 'ci-vitest-e2e',
    E2E_SECRET_TOKEN: SECRET,
    E2E_STATE_DIR: join(dir, 'state'),
    FASTPROOF_SPOOL_DIR: join(dir, 'spool'),
    CI: '',
    ...extra,
  };
  const p = spawn('npx', ['vitest', 'run', '--root', 'e2e'], { cwd: ROOT, env });
  let output = '';
  p.stdout.on('data', (d: Buffer) => (output += d));
  p.stderr.on('data', (d: Buffer) => (output += d));
  const code = await new Promise<number | null>((r) => p.on('close', r));
  return { code, output, dir };
}

describe('vitest with FastProof attached (real Vitest run, fake server)', () => {
  const servers: FakeServer[] = [];
  after(async () => {
    for (const s of servers) await s.stop().catch(() => undefined);
  });
  const ready = existsSync(join(ROOT, 'dist', 'index.js'));

  it('streams results, retries as attempts, logs, attachments and traceparent', { skip: !ready }, async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: true, direct: true }).start();
    servers.push(srv);
    const { code, output } = await run(srv);
    assert.equal(code, 1, output);
    const results = [...srv.results.values()];
    const byName = new Map<string, typeof results>();
    for (const r of results) byName.set(String(r.body.name), [...(byName.get(String(r.body.name)) ?? []), r]);
    const attempts = (n: string) => (byName.get(n) ?? []).sort((a, b) => Number(a.body.retry) - Number(b.body.retry));
    assert.deepEqual(attempts('flaky: passes on third try').map((r) => r.status), ['failed', 'failed', 'passed']);
    assert.deepEqual(attempts('flaky: passes on third try').map((r) => r.body.retry), [0, 1, 2]);
    assert.deepEqual(attempts('fails every time').map((r) => r.status), ['failed', 'failed']);
    assert.equal(attempts('throws a plain error')[0]!.status, 'broken');
    assert.equal(attempts('skipped')[0]!.status, 'skipped');
    assert.equal(attempts('passes with output')[0]!.body.testCaseId, 'CASE-1');
    assert.ok((attempts('passes with output')[0]!.body.labels as { name: string; value: string }[]).some((l) => l.name === 'owner' && l.value === 'team-a'));
    assert.equal(attempts('param 1').length + attempts('param 2').length, 2);
    assert.equal(new Set(attempts('flaky: passes on third try').map((r) => String(r.body.traceparent).split('-')[1])).size, 3);
    assert.equal(attempts('passes with output')[0]!.history[0], 'running');
    const flakyFail = attempts('flaky: passes on third try')[0]!.body;
    assert.match(String((flakyFail.error as { message: string }).message), /expected 0 to be greater than or equal to 2/);
    const dump = JSON.stringify(results) + JSON.stringify(srv.logs) + [...srv.attachments.values()].map((a) => a.body.toString('latin1')).join('');
    assert.ok(!dump.includes(SECRET), 'secret leaked');
    const out = String(attempts('passes with output')[0]!.body.stdout);
    assert.match(out, /starting with token \*\*\*\*/);
    assert.match(String(attempts('passes with output')[0]!.body.stderr), /a warning/);
    const logs = srv.logs.filter((l) => String(l.message).includes('flaky attempt'));
    assert.deepEqual(
      logs.map((l) => [l.attempt, String(l.message)]).sort(),
      [[0, 'flaky attempt 0'], [1, 'flaky attempt 1'], [2, 'flaky attempt 2']],
    );
    const note = [...srv.attachments.values()].filter((a) => a.name === 'note');
    assert.equal(note.length, 1);
    assert.equal(note[0]!.body.toString(), 'body with ****');
    const seen = JSON.parse([...srv.attachments.values()].find((a) => a.name === 'seen-headers.json')!.body.toString()) as { traceparent: string };
    assert.equal(seen.traceparent.split('-')[1], String(attempts('sends traceparent')[0]!.body.traceparent).split('-')[1]);
    const big = [...srv.attachments.values()].find((a) => a.name === 'big.bin');
    assert.equal(big?.body.length, 2 * 1024 * 1024);
    assert.equal(srv.directPuts, 1);
    assert.match(output, /\[fastproof\] launch #1: 12 results/);
  });

  it('runs only the tests of a test plan and reports only those', { skip: !ready }, async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: true }).start();
    servers.push(srv);
    const dir = mkdtempSync(join(tmpdir(), 'fp-vitest-plan-'));
    const plan = join(dir, 'testplan.json');
    writeFileSync(
      plan,
      JSON.stringify({ version: '1.0', tests: [{ id: 'CASE-1' }, { selector: 'tests/api.test.ts#checkout fails every time' }] }),
    );
    const { code, output } = await run(srv, { FASTPROOF_TESTPLAN: plan });
    assert.equal(code, 1, output);
    const names = new Set([...srv.results.values()].map((r) => String(r.body.name)));
    assert.deepEqual([...names].sort(), ['fails every time', 'passes with output']);
  });

  it('writes a spool when the server is down and leaves the exit code alone', { skip: !ready }, async () => {
    const srv = await new FakeServer({ token: TOKEN }).start();
    const url = srv.url;
    await srv.stop();
    const dead = { url } as FakeServer;
    const { code, output, dir } = await run(dead, { FASTPROOF_FLUSH_TIMEOUT_MS: '3000' });
    assert.equal(code, 1, output);
    assert.match(output, /saved to/);
    assert.ok(existsSync(join(dir, 'spool')));
  });
});
