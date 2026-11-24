import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { after, describe, it } from 'node:test';

import { FakeServer } from './fake-server';

const ROOT = resolve(__dirname, '..', '..');
const SECRET = 'jt-secret-xyz-4242';
const TOKEN = 'test-token-abcdef';

async function run(url: string, extra: Record<string, string> = {}, args: string[] = []): Promise<{ code: number | null; output: string; dir: string }> {
  const dir = mkdtempSync(join(tmpdir(), 'fp-jest-e2e-'));
  mkdirSync(join(dir, 'state'));
  const env = {
    ...process.env,
    FASTPROOF_URL: url,
    FASTPROOF_TOKEN: TOKEN,
    FASTPROOF_PROJECT: 'demo',
    FASTPROOF_LAUNCH: 'ci-jest-e2e',
    E2E_SECRET_TOKEN: SECRET,
    E2E_STATE_DIR: join(dir, 'state'),
    FASTPROOF_SPOOL_DIR: join(dir, 'spool'),
    CI: '',
    ...extra,
  };
  const p = spawn('npx', ['jest', '-c', 'e2e/jest.config.js', ...(args.includes('--runInBand=false') ? args.filter((a) => a !== '--runInBand=false') : ['--runInBand', ...args])], { cwd: ROOT, env });
  let output = '';
  p.stdout.on('data', (d: Buffer) => (output += d));
  p.stderr.on('data', (d: Buffer) => (output += d));
  const code = await new Promise<number | null>((r) => p.on('close', r));
  return { code, output, dir };
}

describe('jest with FastProof attached (real Jest run, fake server)', () => {
  const servers: FakeServer[] = [];
  after(async () => {
    for (const s of servers) await s.stop().catch(() => undefined);
  });
  const ready = existsSync(join(ROOT, 'dist', 'index.js'));

  it('streams results, retries as attempts, logs, steps, attachments and traceparent', { skip: !ready }, async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: true, direct: true }).start();
    servers.push(srv);
    const { code, output } = await run(srv.url);
    assert.equal(code, 1, output);
    const results = [...srv.results.values()];
    const byName = new Map<string, typeof results>();
    for (const r of results) byName.set(String(r.body.name), [...(byName.get(String(r.body.name)) ?? []), r]);
    const attempts = (n: string) => (byName.get(n) ?? []).sort((a, b) => Number(a.body.retry) - Number(b.body.retry));
    assert.deepEqual(attempts('flaky: passes on third try').map((r) => r.status), ['failed', 'failed', 'passed']);
    assert.deepEqual(attempts('flaky: passes on third try').map((r) => r.body.retry), [0, 1, 2]);
    assert.deepEqual(attempts('fails every time').map((r) => r.status), ['failed', 'failed', 'failed']);
    assert.equal(attempts('throws a plain error')[0]!.status, 'broken');
    assert.equal(attempts('skipped')[0]!.status, 'skipped');
    assert.equal(attempts('later')[0]!.status, 'skipped');
    assert.equal(attempts('passes with output [fastproof-id:CASE-1]')[0]!.body.testCaseId, 'CASE-1');
    assert.equal(attempts('param 1').length + attempts('param 2').length, 2);
    assert.equal(new Set(attempts('flaky: passes on third try').map((r) => String(r.body.traceparent).split('-')[1])).size, 3);
    assert.equal(attempts('passes with output [fastproof-id:CASE-1]')[0]!.history[0], 'running');
    assert.ok([...srv.results.values()].some((r) => String(r.body.name).includes('suite failed to run') && r.status === 'broken'));
    const dump = JSON.stringify(results) + JSON.stringify(srv.logs) + [...srv.attachments.values()].map((a) => a.body.toString('latin1')).join('');
    assert.ok(!dump.includes(SECRET), 'secret leaked');
    const first = attempts('passes with output [fastproof-id:CASE-1]')[0]!.body;
    assert.match(String(first.stdout), /starting with token \*\*\*\*/);
    assert.match(String(first.stderr), /a warning/);
    assert.ok(srv.logs.some((l) => l.stream === 'step' && String(l.message).includes('inner step') && l.step === 'outer step › inner step'));
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
    assert.equal([...srv.attachments.values()].find((a) => a.name === 'big.bin')?.body.length, 2 * 1024 * 1024);
    assert.equal(srv.directPuts, 1);
    assert.match(output, /\[fastproof\] launch #1: \d+ results/);
  });

  it('works with several worker processes', { skip: !ready }, async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: true }).start();
    servers.push(srv);
    const { output } = await run(srv.url, {}, ['--maxWorkers=2', '--runInBand=false']);
    const names = [...srv.results.values()].map((r) => String(r.body.name));
    assert.ok(names.includes('runs in another worker'), output);
    const r = [...srv.results.values()].find((x) => x.body.name === 'runs in another worker')!;
    assert.equal(r.status, 'passed');
    assert.match(String(r.body.stdout), /hello from the second file/);
    assert.ok(srv.logs.some((l) => String(l.message) === 'hello from the second file'));
  });

  it('reports only the tests of a test plan', { skip: !ready }, async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: true }).start();
    servers.push(srv);
    const dir = mkdtempSync(join(tmpdir(), 'fp-jest-plan-'));
    const plan = join(dir, 'testplan.json');
    writeFileSync(
      plan,
      JSON.stringify({ version: '1.0', tests: [{ id: 'CASE-1' }, { selector: 'tests/api.test.js#checkout fails every time' }] }),
    );
    const { output } = await run(srv.url, { FASTPROOF_TESTPLAN: plan });
    const names = new Set([...srv.results.values()].map((r) => String(r.body.name)));
    assert.deepEqual([...names].sort(), ['fails every time', 'passes with output [fastproof-id:CASE-1]', 'tests/broken.test.js (suite failed to run)'].sort(), output);
  });

  it('writes a spool when the server is down and leaves the exit code alone', { skip: !ready }, async () => {
    const srv = await new FakeServer({ token: TOKEN }).start();
    const url = srv.url;
    await srv.stop();
    const { code, output, dir } = await run(url, { FASTPROOF_FLUSH_TIMEOUT_MS: '3000' });
    assert.equal(code, 1, output);
    assert.match(output, /saved to/);
    assert.ok(existsSync(join(dir, 'spool')));
  });
});
