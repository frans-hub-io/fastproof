import { describe, expect, it } from 'vitest';
import { putSettings, settingsOf } from '../mocks/stageC';
import { stripAnsi } from './ansi';
import { messagePrefix } from './errorGroups';
import { resultNames, sameNameHints } from './names';

describe('console result names', () => {
  it('maps result ids and the client ids of their attempts to the name', () => {
    const m = resultNames([
      { id: 'r1', name: 'test_p[chrome]', clientIds: ['py-a-0', 'py-a-1'] },
      { id: 'r2', name: 'test_p[firefox]' },
    ]);
    expect(m.get('r1')).toBe('test_p[chrome]');
    expect(m.get('py-a-1')).toBe('test_p[chrome]');
    expect(m.get('r2')).toBe('test_p[firefox]');
    expect(m.get('py-b-0')).toBeUndefined();
  });
});

describe('same-name hints', () => {
  it('hints only names shared by several rows, with the id part that differs', () => {
    const items = [
      { name: 'test_fail', tc: 'aaaaaaaa-1111-2222-3333-444444444444', s: 'x.1' },
      { name: 'test_fail', tc: 'bbbbbbbb-1111-2222-3333-444444444444', s: 'y.2' },
      { name: 'test_flaky', tc: 'cccccccc-1111-2222-3333-444444444444', s: 'c.abcdef0123' },
      { name: 'test_flaky', tc: 'cccccccc-1111-2222-3333-444444444444', s: 'c.98765432ff' },
      { name: 'unique', tc: 'dddddddd-1111-2222-3333-444444444444', s: 'd.1' },
    ];
    const h = sameNameHints(items, (i) => i.name, (i) => [i.tc, i.s]);
    expect(items.map((i) => h.get(i) ?? null)).toEqual(['#aaaaaaaa', '#bbbbbbbb', '#abcdef01', '#98765432', null]);
  });
});

describe('ANSI in error messages', () => {
  it('is removed from list titles', () => {
    const raw = 'Error: \u001b[2mexpect(\u001b[22m\u001b[31mlocator\u001b[39m).toBeVisible()';
    expect(stripAnsi(raw)).toBe('Error: expect(locator).toBeVisible()');
    expect(messagePrefix(raw)).not.toContain('\u001b');
  });
});

describe('mock settings PUT', () => {
  it('replaces the areas in the body and keeps the others; null resets', () => {
    const before = settingsOf('web-shop');
    expect(before.gates.length).toBeGreaterThan(0);
    const after = putSettings('web-shop', { notifications: [] });
    expect(after.notifications).toEqual([]);
    expect(after.gates).toEqual(before.gates);
    expect(after.issueTrackers).toEqual(before.issueTrackers);
    const reset = putSettings('web-shop', { fastmock: null, gates: null });
    expect(reset.fastmock ?? null).toBeNull();
    expect(reset.gates).toEqual([]);
    expect(reset.vcs).toEqual(before.vcs);
  });
});
