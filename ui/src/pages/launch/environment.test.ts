import { describe, expect, it } from 'vitest';
import { envKey, envName, NONE_ENV } from './environment';

const counts = { total: 0, passed: 0, failed: 0, broken: 0, skipped: 0, unknown: 0, flaky: 0, muted: 0 };

describe('environment keys in the URL', () => {
  it('writes no environment as the marker and a real key escaped', () => {
    expect(NONE_ENV).toBe('%00');
    expect(envKey({ key: null, counts })).toBe('%00');
    expect(envKey({ counts })).toBe('%00');
    expect(envKey({ key: '(none)', counts })).toBe('(none)');
    expect(envKey({ key: 'a,b', counts })).toBe('a%2Cb');
    expect(envKey({ key: '100%', counts })).toBe('100%25');
    expect(envKey({ key: 'staging', counts })).toBe('staging');
  });

  it('names them back', () => {
    expect(envName('%00')).toBe('No environment');
    expect(envName('(none)')).toBe('(none)');
    expect(envName('a%2Cb')).toBe('a,b');
  });
});
