import { describe, expect, it } from 'vitest';
import { matchesQuery, queryProblem } from './queries';
import { tmState } from './tm';

describe('mock FPQL', () => {
  it('accepts the documented grammar', () => {
    const q =
      'status = active and automated = false and field["Priority"] in ["high", "critical"] and folder in subtree("Payments/Cards") and not tag = legacy and (lastStatus in [failed, broken] or lastRunAt < now() - 14d) and requirement = REQ-12 or sharedStep = SS-3 or filter = "Regression core" and tag is not empty';
    expect(queryProblem(q)).toBeNull();
    expect(queryProblem('')).toBeNull();
  });

  it('points at an unknown attribute', () => {
    expect(queryProblem('status = active and colour = red')).toMatchObject({ position: 20, length: 6 });
  });

  it('points at an unclosed string and parenthesis', () => {
    expect(queryProblem('name = "abc')).toMatchObject({ position: 7, length: 4 });
    expect(queryProblem('(status = active')).toMatchObject({ position: 16, length: 0, expected: [')'] });
    expect(queryProblem('status = active)')).toMatchObject({ position: 15 });
  });

  it('rejects ~ with fewer than three characters', () => {
    expect(queryProblem('name ~ "ab"')).toMatchObject({ position: 7, message: '~ needs at least 3 characters' });
  });

  it('filters the mock cases by status and automation', () => {
    const cases = [...tmState().cases.values()].filter((c) => !c.archivedAt);
    const active = cases.filter((c) => matchesQuery('status = active', c));
    expect(active.length).toBeGreaterThan(0);
    expect(active.every((c) => c.status === 'active')).toBe(true);
    const either = cases.filter((c) => matchesQuery('status in [draft, outdated]', c));
    expect(either.every((c) => c.status !== 'active')).toBe(true);
    const manual = cases.filter((c) => matchesQuery('automated = false', c));
    expect(manual.every((c) => !c.identities.some((i) => i.kind === 'code'))).toBe(true);
    expect(active.length + cases.filter((c) => matchesQuery('not status = active', c)).length).toBe(cases.length);
  });
});
