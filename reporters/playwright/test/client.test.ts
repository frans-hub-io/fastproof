import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, describe, it } from 'node:test';

import { Api } from '../src/api';
import { Client } from '../src/client';
import { resolveConfig } from '../src/config';
import { MASK } from '../src/mask';
import { SpoolSession, flushSpool, listSessions } from '../src/spool';
import type { ReportResult } from '../src/types';
import { FakeServer } from './fake-server';

const TOKEN = 'test-token-abcdef';

function config(url: string, extra: Record<string, unknown> = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'fp-client-'));
  return resolveConfig(
    { url, token: TOKEN, project: 'demo', spoolDir: join(dir, 'spool'), flushTimeoutMs: 5000, batchIntervalMs: 20, ...extra },
    { SHOP_PASSWORD: 'correct-horse-battery' },
    dir,
  );
}

function result(id: string, status: ReportResult['status'] = 'passed'): ReportResult {
  return { id, name: id, fullName: `suite › ${id}`, status, start: Date.now(), stop: Date.now() + 1, retry: 0 };
}

const quiet: string[] = [];
const out = (m: string) => void quiet.push(m);

describe('client against a live server', () => {
  const servers: FakeServer[] = [];
  after(async () => {
    for (const s of servers) await s.stop().catch(() => undefined);
  });

  it('opens running results, streams logs, dedups attachments and finishes its own launch', async () => {
    const srv = await new FakeServer({ token: TOKEN, logs: true, running: true }).start();
    servers.push(srv);
    const c = new Client(config(srv.url), 'test', out);
    c.start();
    c.openRunning(result('r1'));
    c.log('r1', 0, 'info', 'stdout', 'hello correct-horse-battery');
    c.log('r1', 0, 'info', 'step', 'step one', 'outer');
    const dir = mkdtempSync(join(tmpdir(), 'fp-att-'));
    const shot = join(dir, 'shot.png');
    writeFileSync(shot, Buffer.from([1, 2, 3, 4]));
    c.finish(result('r1'), [
      { name: 'screenshot', contentType: 'image/png', path: shot },
      { name: 'note correct-horse-battery', contentType: 'text/plain', body: Buffer.from('pw=correct-horse-battery') },
    ], true);
    c.finish({ ...result('r2', 'failed'), retry: 0 }, [{ name: 'screenshot', contentType: 'image/png', path: shot }], false);
    const s = await c.close('finished');
    assert.equal(s.sent, 2);
    assert.equal(s.logs, 'on');
    assert.equal(s.running, true);
    assert.equal(s.spooled, 0);
    assert.deepEqual(srv.results.get('r1')?.history, ['running', 'passed']);
    assert.ok(srv.order.indexOf('r1:running') < srv.order.indexOf('r1:passed'));
    assert.equal(srv.uploads, 2, 'the same screenshot is uploaded once');
    const r1 = srv.results.get('r1')!.body as { attachments: { name: string }[] };
    assert.equal(r1.attachments.length, 2);
    const note = [...srv.attachments.values()].find((a) => a.contentType === 'text/plain')!;
    assert.equal(note.body.toString(), `pw=${MASK}`);
    assert.equal(srv.logs.length, 2);
    assert.deepEqual(srv.logs.map((l) => l.seq), [0, 1]);
    assert.equal(srv.logs[0]!.message, `hello ${MASK}`);
    assert.equal(srv.logs[1]!.step, 'outer');
    assert.equal([...srv.launches.values()][0]!.finished, 'finished');
  });

  it('degrades on a server without running results and logs (stage B)', async () => {
    const srv = await new FakeServer({ logs: false, running: false }).start();
    servers.push(srv);
    const c = new Client(config(srv.url, { finishLaunch: 'never' }), 'test', out);
    c.start();
    c.openRunning(result('a'));
    c.log('a', 0, 'info', 'stdout', 'x');
    await c.pump();
    c.openRunning(result('b'));
    c.finish(result('a'), [], true);
    c.finish(result('b'), [], true);
    const s = await c.close('finished');
    assert.equal(s.sent, 2);
    assert.equal(s.logs, 'off');
    assert.equal(s.running, false);
    assert.deepEqual(srv.order, ['a:passed', 'b:passed']);
    assert.equal(srv.logs.length, 0);
    assert.equal([...srv.launches.values()][0]!.finished, undefined);
  });

  it('retries 5xx without losing results', async () => {
    const srv = await new FakeServer({ running: true }).start();
    servers.push(srv);
    const c = new Client(config(srv.url), 'test', out);
    c.start();
    await c.pump();
    srv.failNext = 2;
    c.finish(result('x'), [], true);
    const s = await c.close('finished');
    assert.equal(s.sent, 1);
    assert.equal(srv.results.get('x')?.status, 'passed');
  });

  it('spools when the server is down and flushes later with attachments', async () => {
    const probe = await new FakeServer().start();
    const port = Number(new URL(probe.url).port);
    await probe.stop();
    const cfg = config(`http://127.0.0.1:${port}`, { flushTimeoutMs: 2000 });
    const c = new Client(cfg, 'test', out);
    c.start();
    c.openRunning(result('o1'));
    const dir = mkdtempSync(join(tmpdir(), 'fp-att-'));
    const trace = join(dir, 'trace.zip');
    writeFileSync(trace, Buffer.alloc(1024, 7));
    c.finish(result('o1', 'failed'), [{ name: 'trace', contentType: 'application/zip', path: trace }], false);
    c.finish(result('o2'), [], true);
    const s = await c.close('finished');
    assert.equal(s.offline, true);
    assert.equal(s.spooled, 2);
    assert.ok(s.spoolDir && existsSync(join(s.spoolDir, 'results.ndjson')));
    assert.equal(readdirSync(join(s.spoolDir, 'blobs')).length, 1);
    assert.equal(listSessions(cfg.spoolDir).length, 1);

    const srv = await new FakeServer({ token: TOKEN, port }).start();
    servers.push(srv);
    const r = await flushSpool(new Api(srv.url, TOKEN, 'demo'), cfg.spoolDir);
    assert.equal(r.results, 2);
    assert.equal(r.remaining, 0);
    assert.equal(srv.results.get('o1')?.status, 'failed');
    const o1 = srv.results.get('o1')!.body as { attachments: { id: string; name: string }[] };
    assert.equal(o1.attachments[0]!.name, 'trace');
    assert.equal(srv.uploads, 1);
    const launch = [...srv.launches.values()][0]!;
    assert.equal(launch.externalId, cfg.launch.externalId);
    assert.equal(launch.finished, 'finished');
    assert.equal(listSessions(cfg.spoolDir).length, 0);
    const again = await flushSpool(new Api(srv.url, TOKEN, 'demo'), cfg.spoolDir);
    assert.equal(again.results, 0);
  });

  it('keeps half of the spool for failures', () => {
    const root = mkdtempSync(join(tmpdir(), 'fp-spool-'));
    const s = new SpoolSession(
      root,
      's1',
      { v: 1, reporter: 't', project: 'demo', launch: { externalId: 'e', name: 'n' }, createdAt: '', pid: 1, host: 'h' },
      1000,
    );
    assert.equal(s.addBlob('a'.repeat(64), { body: Buffer.alloc(600) }, 600, true), false);
    assert.equal(s.addBlob('b'.repeat(64), { body: Buffer.alloc(600) }, 600, false), true);
    assert.equal(s.addBlob('c'.repeat(64), { body: Buffer.alloc(600) }, 600, false), false);
    assert.equal(s.append({ v: 1, result: result('z'), blobs: [] }), true);
    assert.equal(s.dropped.blobs, 2);
    assert.ok(readFileSync(join(root, 's1', 'results.ndjson'), 'utf8').includes('"z"'));
  });
});
