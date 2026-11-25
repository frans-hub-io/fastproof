import { describe, expect, it } from 'vitest';
import { hasLogCondition, isAfterLaunch, logDraftOf, logMatchOf, logTextError, patternError } from './logDraft';

describe('log rule drafts', () => {
  it('round-trips match.log and keeps signals', () => {
    const d = logDraftOf({ log: { regex: 'OOM|heap', levels: ['error'], streams: ['stderr'], lastBytes: 4096 }, signals: [{ source: 'grid', kind: 'node-lost' }] }, 'oom');
    expect(d.logMode).toBe('regex');
    expect(d.logLastKiB).toBe(4);
    expect(logMatchOf(d)).toEqual({ log: { regex: 'OOM|heap', contains: undefined, levels: ['error'], streams: ['stderr'], lastBytes: 4096 }, signals: [{ source: 'grid', kind: 'node-lost' }] });
    expect(isAfterLaunch(d)).toBe(true);
  });

  it('leaves the log out when it is off or empty', () => {
    const d = { ...logDraftOf(undefined, null), logMode: 'contains' as const, logText: '  ' };
    expect(logMatchOf(d)).toEqual({ log: undefined, signals: undefined });
    expect(hasLogCondition(d)).toBe(false);
    expect(isAfterLaunch(d)).toBe(false);
  });

  it('checks pattern names and log text', () => {
    expect(patternError('db-timeout.v2_x')).toBeNull();
    expect(patternError('Bad')).not.toBeNull();
    expect(patternError('-x')).not.toBeNull();
    const d = { ...logDraftOf(undefined, null), logMode: 'contains' as const, logText: 'x'.repeat(1025) };
    expect(logTextError(d, () => null)).toContain('1024');
    expect(logTextError({ ...d, logMode: 'regex', logText: '(' }, () => 'bad')).toBe('bad');
  });
});
