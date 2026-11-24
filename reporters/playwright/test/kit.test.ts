import assert from 'node:assert/strict';
import { test } from 'node:test';

import { ReporterSession, RUN_ENV, shortHash } from '../src/kit';
import { LAUNCH_ENV, SEED_ENV } from '../src/trace';

test('a session shares seed, launch and run id with worker processes through the environment', () => {
  const env: Record<string, string | undefined> = { FASTPROOF_URL: 'http://127.0.0.1:1', FASTPROOF_PROJECT: 'demo', FASTPROOF_LAUNCH: 'l-1' };
  const s = new ReporterSession({}, 'x', 'xx', () => undefined, env);
  assert.equal(env[SEED_ENV], s.seed);
  assert.equal(env[LAUNCH_ENV], 'l-1');
  assert.equal(env[RUN_ENV], s.runId);
  assert.match(s.resultId('file\u0000test', 2), /^xx-[0-9a-f]{12}-[0-9a-f]{16}-2$/);
  assert.equal(s.resultId('a', 0), s.resultId('a', 0));
  assert.notEqual(s.resultId('a', 0), s.resultId('a', 1));
  assert.equal(s.trace('a', 1).traceparent, s.trace('a', 1).traceparent);
  assert.notEqual(s.trace('a', 1).traceId, s.trace('a', 2).traceId);
  assert.equal(shortHash('a').length, 16);
});

test('a disabled session leaves the environment alone and reports why once', () => {
  const env: Record<string, string | undefined> = {};
  const out: string[] = [];
  const s = new ReporterSession({}, 'x', 'xx', (m) => out.push(m), env);
  assert.equal(s.start(), false);
  assert.equal(env[SEED_ENV], undefined);
  assert.match(out.join(''), /FASTPROOF_URL is not set/);
  assert.equal(s.guard(() => assert.fail('must not run')), undefined);
});
