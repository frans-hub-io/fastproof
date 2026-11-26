import { describe, expect, it } from 'vitest';
import { ApiError } from './client';
import {
  coverageOfLinks,
  coverageStatusOf,
  launchIdList,
  parseCaseNumbers,
  parseRequirementNumber,
  queryErrorOf,
  requirementError,
  scopeFromParams,
  scopeQuery,
  scopeReady,
  scopeToParams,
  type RequirementLink,
} from './requirements';

const link = (status: string | null, linkStatus: RequirementLink['linkStatus'] = 'active'): Pick<RequirementLink, 'linkStatus' | 'lastResult'> => ({
  linkStatus,
  lastResult: status ? { status, at: '2026-01-01T00:00:00Z', source: 'automated', launchId: 'l', resultId: 'r' } : null,
});

describe('requirement numbers', () => {
  it('reads REQ-12, req12, #12 and 12', () => {
    for (const t of ['REQ-12', 'req12', '#12', ' 12 ']) expect(parseRequirementNumber(t)).toBe(12);
    expect(parseRequirementNumber('TC-12')).toBeNull();
    expect(parseRequirementNumber('')).toBeNull();
  });

  it('splits case numbers, drops duplicates and reports the rest', () => {
    expect(parseCaseNumbers('TC-1, 2 3;2 #4')).toEqual({ numbers: [1, 2, 3, 4], invalid: [] });
    expect(parseCaseNumbers('1 abc 2x')).toEqual({ numbers: [1], invalid: ['abc', '2x'] });
  });
});

describe('coverage of links', () => {
  it('has no coverage without active links', () => {
    expect(coverageOfLinks([]).status).toBe('uncovered');
    expect(coverageOfLinks([link('passed', 'proposed')]).status).toBe('uncovered');
  });

  it('fails when one result fails, passes when all pass, is not run without results, partial otherwise', () => {
    expect(coverageOfLinks([link('passed'), link('broken')]).status).toBe('failed');
    expect(coverageOfLinks([link('passed'), link('passed')]).status).toBe('passed');
    expect(coverageOfLinks([link(null), link(null)]).status).toBe('notRun');
    expect(coverageOfLinks([link('passed'), link(null), link('skipped')]).status).toBe('partial');
    expect(coverageOfLinks([link('passed'), link(null), link('skipped')]).counts).toEqual({ total: 3, passed: 1, failed: 0, notRun: 1, skipped: 1 });
  });

  it('judges counts the same way', () => {
    expect(coverageStatusOf({ total: 0, passed: 0, failed: 0, notRun: 0, skipped: 0 })).toBe('uncovered');
    expect(coverageStatusOf({ total: 2, passed: 1, failed: 1, notRun: 0, skipped: 0 })).toBe('failed');
  });
});

describe('coverage scope in the URL', () => {
  it('keeps nothing for the default scope and round-trips the others', () => {
    expect(scopeToParams({ kind: 'default' })).toEqual({});
    for (const s of [
      { kind: 'plan', planId: 'p1', launchId: 'l1' },
      { kind: 'launches', launchIds: 'a,b' },
      { kind: 'window', days: 14, branch: 'main', environment: 'qa' },
    ] as const) expect(scopeFromParams(new URLSearchParams(scopeToParams(s)))).toEqual(s);
  });

  it('falls back to 30 days for a bad window and to the default scope for an unknown one', () => {
    expect(scopeFromParams(new URLSearchParams('scope=window&days=900')).days).toBe(30);
    expect(scopeFromParams(new URLSearchParams('scope=nonsense')).kind).toBe('default');
  });

  it('asks for what the scope needs', () => {
    expect(scopeReady({ kind: 'plan' })).toBe(false);
    expect(scopeReady({ kind: 'launches', launchIds: ' , ' })).toBe(false);
    expect(scopeReady({ kind: 'window', days: 7 })).toBe(true);
    expect(launchIdList('a, b;c')).toEqual(['a', 'b', 'c']);
    expect(scopeQuery({ kind: 'launches', launchIds: 'a, b' })).toEqual({ launchIds: 'a,b' });
    expect(scopeQuery({ kind: 'window', days: 7, branch: 'main' })).toEqual({ days: 7, branch: 'main', environment: undefined });
    expect(scopeQuery({ kind: 'default' })).toEqual({});
  });
});

describe('error messages', () => {
  it('explains the conflicts of requirements and keeps the server text otherwise', () => {
    const e = (status: number, code: string, message: string, details?: unknown) => new ApiError(status, { code, message, details });
    expect(requirementError(e(409, 'requirement_cycle', 'x'))).toMatch(/cannot move under itself/);
    expect(requirementError(e(409, 'issue_key_taken', 'x'))).toMatch(/issue key/);
    expect(requirementError(e(409, 'limit_reached', 'too many links', { limit: 'max_links', max: 500 }))).toBe('too many links (at most 500)');
    expect(requirementError(e(404, 'not_found', 'case TC-9 not found'))).toBe('case TC-9 not found');
  });

  it('reads the position of a bad query', () => {
    const err = new ApiError(400, { code: 'invalid_query', message: 'bad', details: { position: 4, length: 2, message: 'unknown attribute' } });
    expect(queryErrorOf(err)).toEqual({ message: 'unknown attribute', position: 4, length: 2 });
    expect(queryErrorOf(new Error('x'))).toBeNull();
  });
});
