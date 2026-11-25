import { describe, expect, it } from 'vitest';
import type { CaseStep } from '../../api/tm';
import { adjacentRun, draftProblem } from './shared';

const s = (id: string, shared = false): CaseStep => ({ id, action: id, expected: '', data: '', attachments: [], steps: [], ...(shared ? { shared: { id: 'x', number: 1 } } : {}) });

describe('adjacentRun', () => {
  const steps = [s('a'), s('b'), s('c'), s('d', true), s('e')];
  it('returns the chosen steps in case order when they touch', () => {
    expect(adjacentRun(steps, new Set(['c', 'b']))).toEqual(['b', 'c']);
    expect(adjacentRun(steps, new Set(['a']))).toEqual(['a']);
  });
  it('refuses nothing, gaps, unknown ids and shared references', () => {
    expect(adjacentRun(steps, new Set())).toBeNull();
    expect(adjacentRun(steps, new Set(['a', 'c']))).toBeNull();
    expect(adjacentRun(steps, new Set(['a', 'zzz']))).toBeNull();
    expect(adjacentRun(steps, new Set(['c', 'd']))).toBeNull();
  });
});

describe('draftProblem', () => {
  it('asks for a name and a step', () => {
    expect(draftProblem({ name: ' ', description: '', steps: [s('a')] })).toMatch(/name/);
    expect(draftProblem({ name: 'x', description: '', steps: [] })).toMatch(/step/);
    expect(draftProblem({ name: 'x', description: '', steps: [s('a')] })).toBeNull();
  });
});
