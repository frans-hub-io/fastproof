import { describe, expect, it } from 'vitest';
import type { CaseSnapshot, CaseStep, StepStatus } from '../../api/tm';
import { parseCaseNumber, readListState, toCaseFilter, toCaseQuery, writeListState } from './filters';
import { effectiveStatus, effectiveStatuses, flattenSteps, indentStep, insertAfter, moveStep, outdentStep, suggestedStatus, untestedLeaves, worstStatus } from './steps';
import { diffSnapshots } from './versionDiff';

function s(id: string, action = id, children: CaseStep[] = []): CaseStep {
  return { id, action, expected: `${action} works`, data: '', attachments: [], steps: children };
}

const tree: CaseStep[] = [s('a'), s('b', 'b', [s('b1'), s('b2'), s('b3', 'b3', [s('b3x')])]), s('c')];

function results(entries: [string, StepStatus][]): Map<string, StepStatus> {
  return new Map(entries);
}

describe('step tree', () => {
  it('flattens with 1-based paths', () => {
    expect(flattenSteps(tree).map((f) => f.path)).toEqual(['1', '2', '2.1', '2.2', '2.3', '2.3.1', '3']);
  });

  it('inserts, moves, indents and outdents keeping ids', () => {
    const ins = insertAfter(tree, '2.1', s('new'));
    expect(ins.path).toBe('2.2');
    expect(flattenSteps(ins.steps).map((f) => f.step.id)).toEqual(['a', 'b', 'b1', 'new', 'b2', 'b3', 'b3x', 'c']);
    const up = moveStep(tree, '3', -1);
    expect(up?.path).toBe('2');
    expect(up?.steps.map((x) => x.id)).toEqual(['a', 'c', 'b']);
    expect(moveStep(tree, '1', -1)).toBeNull();
    const ind = indentStep(tree, '3');
    expect(ind?.path).toBe('2.4');
    expect(indentStep(tree, '1')).toBeNull();
    expect(indentStep(tree, '2.3')).toBeNull();
    const out = outdentStep(tree, '2.2');
    expect(out?.path).toBe('3');
    expect(out?.steps.map((x) => x.id)).toEqual(['a', 'b', 'b2', 'c']);
    expect(outdentStep(tree, '1')).toBeNull();
  });
});

describe('step status derivation', () => {
  it('a parent takes the worst child status', () => {
    const r = results([
      ['2.1', 'passed'],
      ['2.2', 'failed'],
      ['2.3.1', 'blocked'],
    ]);
    expect(effectiveStatus(tree[1] as CaseStep, '2', r)).toBe('failed');
    expect(worstStatus(['passed', 'skipped'])).toBe('skipped');
    expect(worstStatus(['blocked', 'skipped', 'passed'])).toBe('blocked');
  });

  it('ignores untested children while others exist', () => {
    const r = results([['2.1', 'passed']]);
    expect(effectiveStatus(tree[1] as CaseStep, '2', r)).toBe('passed');
    expect(effectiveStatus(tree[1] as CaseStep, '2', results([]))).toBe('untested');
    expect(effectiveStatuses(tree, r).get('2.3')).toBe('untested');
  });

  it('an explicit parent status overrides the derived one', () => {
    const r = results([
      ['2', 'passed'],
      ['2.2', 'failed'],
    ]);
    expect(effectiveStatus(tree[1] as CaseStep, '2', r)).toBe('passed');
  });

  it('lists untested leaves', () => {
    expect(untestedLeaves(tree, results([['2.1', 'passed']]))).toEqual(['1', '2.2', '2.3.1', '3']);
  });
});

describe('suggested final status', () => {
  it('failed beats blocked beats the rest', () => {
    expect(suggestedStatus(tree, results([['1', 'blocked'], ['2.1', 'failed'], ['3', 'passed']]))).toBe('failed');
    expect(suggestedStatus(tree, results([['1', 'blocked'], ['2.1', 'passed'], ['3', 'passed']]))).toBe('blocked');
  });

  it('all passed is passed; untested left is in progress unless they become skipped', () => {
    const allPassed = results([['1', 'passed'], ['2.1', 'passed'], ['3', 'passed']]);
    expect(suggestedStatus(tree, allPassed)).toBe('passed');
    const partial = results([['1', 'passed']]);
    expect(suggestedStatus(tree, partial)).toBe('in_progress');
    expect(suggestedStatus(tree, partial, true)).toBe('passed');
    expect(suggestedStatus(tree, results([]))).toBe('untested');
    expect(suggestedStatus(tree, results([]), true)).toBe('skipped');
    expect(suggestedStatus([], results([]))).toBe('untested');
  });

  it('passed with skipped is passed, all skipped is skipped', () => {
    expect(suggestedStatus(tree, results([['1', 'passed'], ['2', 'skipped'], ['3', 'skipped']]))).toBe('passed');
    expect(suggestedStatus(tree, results([['1', 'skipped'], ['2', 'skipped'], ['3', 'skipped']]))).toBe('skipped');
  });
});

describe('version diff', () => {
  const base: CaseSnapshot = {
    name: 'Pay',
    status: 'active',
    folderId: null,
    body: { description: 'd', preconditions: '', steps: [s('x', 'open'), s('y', 'pay'), s('z', 'check')], links: [] },
    fields: { priority: 'High', component: ['Cart'] },
    tags: ['a'],
  };

  it('matches steps by id: changed, moved, added and removed', () => {
    const next: CaseSnapshot = {
      ...base,
      name: 'Pay with card',
      body: { ...base.body, steps: [s('y', 'pay'), { ...s('x', 'open'), expected: 'opens fast' }, s('n', 'new')] },
      fields: { priority: 'Critical', component: ['Cart'] },
      tags: ['a', 'b'],
    };
    const d = diffSnapshots(base, next, { priority: 'Priority' });
    expect(d.same).toBe(false);
    expect(d.fields.map((f) => f.key)).toEqual(['name', 'tags', 'fields.priority']);
    expect(d.fields.find((f) => f.key === 'fields.priority')).toMatchObject({ label: 'Priority', before: 'High', after: 'Critical' });
    const byId = new Map(d.steps.map((x) => [x.id, x] as const));
    expect(byId.get('y')).toMatchObject({ kind: 'moved', beforePath: '2', afterPath: '1' });
    expect(byId.get('x')).toMatchObject({ kind: 'changed', beforePath: '1', afterPath: '2' });
    expect(byId.get('x')?.parts).toEqual([{ part: 'expected', before: 'open works', after: 'opens fast' }]);
    expect(byId.get('n')?.kind).toBe('added');
    expect(byId.get('z')).toMatchObject({ kind: 'removed', beforePath: '3', afterPath: null });
  });

  it('reports identical snapshots as the same', () => {
    expect(diffSnapshots(base, structuredClone(base)).same).toBe(true);
  });

  it('a renamed step with the same id is changed, not removed and added', () => {
    const next = { ...base, body: { ...base.body, steps: [s('x', 'open the shop'), s('y', 'pay'), s('z', 'check')] } };
    const d = diffSnapshots(base, next);
    expect(d.steps.filter((x) => x.kind !== 'same').map((x) => [x.id, x.kind])).toEqual([['x', 'changed']]);
  });
});

describe('case list state in the URL', () => {
  it('round-trips and builds the query and the plan filter', () => {
    const sp = writeListState(new URLSearchParams(), { folder: 'f1', statuses: ['active'], automation: 'manual', fields: ['priority:High', 'priority:Critical'], tags: ['smoke'], q: 'pay' });
    const st = readListState(sp);
    expect(st).toMatchObject({ folder: 'f1', recursive: true, statuses: ['active'], automation: 'manual', fields: ['priority:High', 'priority:Critical'], tags: ['smoke'], q: 'pay' });
    expect(toCaseQuery(st)).toMatchObject({ folder: 'f1', recursive: true, status: 'active', field: ['priority:High', 'priority:Critical'] });
    expect(toCaseFilter(st)).toEqual({ folderIds: ['f1'], recursive: true, statuses: ['active'], automation: 'manual', fields: { priority: ['High', 'Critical'] }, tags: ['smoke'], q: 'pay' });
  });

  it('parses case numbers', () => {
    expect(parseCaseNumber('TC-12')).toBe(12);
    expect(parseCaseNumber('tc12')).toBe(12);
    expect(parseCaseNumber('#7')).toBe(7);
    expect(parseCaseNumber('pay')).toBeNull();
  });
});
