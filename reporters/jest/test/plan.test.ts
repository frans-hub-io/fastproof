import assert from 'node:assert/strict';
import { test } from 'node:test';

import { testPlanPattern } from '../src/testplan';

test('the test name pattern covers selectors and ids', () => {
  const p = testPlanPattern({
    version: '1.0',
    tests: [{ id: 'CASE-1' }, { selector: 'tests/api.test.js#checkout fails (every) time' }, { selector: 'tests/x.test.js › a › b' }],
  })!;
  const re = new RegExp(p);
  assert.ok(re.test('checkout fails (every) time'));
  assert.ok(re.test('anything [fastproof-id:CASE-1]'));
  assert.ok(re.test('a b'));
  assert.ok(!re.test('checkout fails'));
  assert.ok(!re.test('[fastproof-id:CASE-10]'));
});

test('no plan means no pattern, an empty plan matches nothing', () => {
  assert.equal(testPlanPattern(undefined), undefined);
  assert.ok(!new RegExp(testPlanPattern({ tests: [] })!).test('x'));
});
