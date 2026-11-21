import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, it } from 'node:test';

import { detectCi } from '../src/ci';
import { resolveConfig } from '../src/config';
import { MASK, Masker } from '../src/mask';
import { statusOf, stripAnsi } from '../src/reporter';
import { inTestPlan, loadTestPlan, testPlanGrep } from '../src/testplan';
import { formatBaggage, parseTraceparent, traceFor } from '../src/trace';

describe('mask', () => {
  it('masks values of secret-looking env vars, explicit values and patterns', () => {
    const m = new Masker(
      { values: ['hunter2-explicit'], patterns: [/card=\d{4}/] },
      { API_TOKEN: 's3cr3t-value', DB_PASSWORD: 'p@ss word', PLAIN: 'visible-value', FEATURE_TOKEN: 'true', X_KEY: '12' },
    );
    const s = m.mask('t=s3cr3t-value p=p@ss word e=hunter2-explicit card=1234 v=visible-value f=true k=12 u=p%40ss%20word');
    assert.equal(s, `t=${MASK} p=${MASK} e=${MASK} ${MASK} v=visible-value f=true k=12 u=${MASK}`);
    assert.ok(m.active);
  });

  it('is inactive without secrets', () => {
    const m = new Masker({ env: [] }, {});
    assert.equal(m.active, false);
    assert.equal(m.mask('abc'), 'abc');
  });

  it('prefers the longest value', () => {
    const m = new Masker({ env: ['A', 'B'] }, { A: 'abcd', B: 'abcdefgh' });
    assert.equal(m.mask('xabcdefghx'), `x${MASK}x`);
  });
});

describe('trace', () => {
  it('derives the same ids from the same seed and differs per attempt', () => {
    const a = traceFor('seed', 'L', 'test-1', 0);
    const b = traceFor('seed', 'L', 'test-1', 0);
    const c = traceFor('seed', 'L', 'test-1', 1);
    assert.equal(a.traceparent, b.traceparent);
    assert.notEqual(a.traceId, c.traceId);
    assert.match(a.traceparent, /^00-[0-9a-f]{32}-[0-9a-f]{16}-01$/);
    assert.deepEqual(parseTraceparent(a.traceparent), { traceId: a.traceId, spanId: a.spanId });
    assert.equal(parseTraceparent('00-00000000000000000000000000000000-0000000000000001-01'), undefined);
    assert.equal(a.baggage, 'fastproof.launch=L,fastproof.test=test-1,fastproof.attempt=0');
  });

  it('is random without a seed and encodes baggage values', () => {
    assert.notEqual(traceFor(undefined, 'L', 't', 0).traceId, traceFor(undefined, 'L', 't', 0).traceId);
    assert.equal(formatBaggage('a b,c', 't', 2), 'fastproof.launch=a%20b%2Cc,fastproof.test=t,fastproof.attempt=2');
  });
});

describe('test plan', () => {
  const plan = {
    version: '1.0',
    tests: [{ selector: 'a.spec.ts › suite › works' }, { selector: 'b.spec.ts#other test' }, { id: 42 }],
  };

  it('builds a grep over title paths', () => {
    const g = testPlanGrep(plan)!;
    assert.ok(g.test('chromium a.spec.ts suite works'));
    assert.ok(g.test('a.spec.ts suite works @smoke'));
    assert.ok(!g.test('a.spec.ts suite works too'));
    assert.ok(g.test('b.spec.ts other test'));
    assert.ok(!g.test('c.spec.ts x'));
    assert.equal(testPlanGrep(undefined), undefined);
    assert.ok(!testPlanGrep({ tests: [{ id: 1 }] })!.test('anything'));
  });

  it('matches by id annotation and by selector forms', () => {
    const t = (titlePath: string[], annotations: { type: string; description?: string }[] = [], line = 3) => ({
      titlePath,
      line,
      column: 5,
      annotations,
    });
    assert.ok(inTestPlan(plan, t(['a.spec.ts', 'suite', 'works'])));
    assert.ok(inTestPlan(plan, t(['b.spec.ts', 'other test'])));
    assert.ok(inTestPlan(plan, t(['z.spec.ts', 'x'], [{ type: 'allure_id', description: '42' }])));
    assert.ok(!inTestPlan(plan, t(['z.spec.ts', 'x'])));
    assert.ok(inTestPlan({ tests: [{ selector: 'z.spec.ts:3' }] }, t(['z.spec.ts', 'x'])));
  });

  it('loads from FASTPROOF_TESTPLAN or ALLURE_TESTPLAN_PATH', () => {
    const dir = mkdtempSync(join(tmpdir(), 'fp-plan-'));
    const f = join(dir, 'testplan.json');
    writeFileSync(f, JSON.stringify(plan));
    assert.equal(loadTestPlan(undefined, { ALLURE_TESTPLAN_PATH: f })?.tests.length, 3);
    assert.equal(loadTestPlan(undefined, { FASTPROOF_TESTPLAN: join(dir, 'missing') }), undefined);
    assert.equal(loadTestPlan(undefined, {}), undefined);
  });
});

describe('ci and config', () => {
  it('detects GitLab, GitHub and Jenkins like the CLI', () => {
    assert.equal(detectCi({ GITLAB_CI: 'true', CI_PROJECT_ID: '42', CI_PIPELINE_ID: '1001' })?.externalId, 'gitlab-42-1001');
    assert.equal(
      detectCi({ GITHUB_ACTIONS: 'true', GITHUB_REPOSITORY: 'o/r', GITHUB_RUN_ID: '9', GITHUB_RUN_ATTEMPT: '2' })?.externalId,
      'github-o/r-9-2',
    );
    assert.equal(detectCi({ JENKINS_URL: 'x', JOB_NAME: 'j', BUILD_NUMBER: '5' })?.ci.buildOrder, 5);
    assert.equal(detectCi({}), undefined);
  });

  it('reads the environment, options win, and turns off without a URL', () => {
    const env = { FASTPROOF_URL: 'http://198.51.100.7', FASTPROOF_PROJECT: 'shop', FASTPROOF_TOKEN: 'tok-123456' };
    const c = resolveConfig({ launch: 'ext-1' }, env, '/work');
    assert.equal(c.enabled, true);
    assert.equal(c.launch.externalId, 'ext-1');
    assert.equal(c.ownLaunch, false);
    assert.equal(c.spoolDir, '/work/.fastproof/spool');
    assert.ok(c.secrets.values?.includes('tok-123456'));
    const own = resolveConfig({}, env, '/work');
    assert.equal(own.ownLaunch, true);
    assert.match(own.launch.externalId, /^local-/);
    assert.equal(resolveConfig({}, { FASTPROOF_PROJECT: 'shop' }).enabled, false);
    assert.equal(resolveConfig({}, { ...env, FASTPROOF_ENABLED: 'false' }).enabled, false);
    assert.equal(resolveConfig({}, { ...env, FASTPROOF_PROJECT: 'Bad Key' }).enabled, false);
    assert.equal(resolveConfig({}, { ...env, GITLAB_CI: '1', CI_PROJECT_ID: '1', CI_PIPELINE_ID: '2' }).ownLaunch, false);
  });
});

describe('status mapping', () => {
  it('follows the expected status', () => {
    assert.equal(statusOf({ expectedStatus: 'passed' }, { status: 'timedOut' }), 'failed');
    assert.equal(statusOf({ expectedStatus: 'failed' }, { status: 'failed' }), 'passed');
    assert.equal(statusOf({ expectedStatus: 'failed' }, { status: 'passed' }), 'failed');
    assert.equal(statusOf({ expectedStatus: 'passed' }, { status: 'interrupted' }), 'broken');
    assert.equal(statusOf({ expectedStatus: 'skipped' }, { status: 'skipped' }), 'skipped');
    assert.equal(stripAnsi('\u001b[31mred\u001b[39m'), 'red');
  });
});
