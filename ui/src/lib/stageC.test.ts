import { describe, expect, it } from 'vitest';
import { appendEntries, parseGapEvent, parseLogEvent, toLogLine, type LogEntry } from '../api/logs';
import { withSettingsDefaults } from '../api/stageC';
import { cspNonce, resetCspNonceCache } from './cspNonce';
import { defectColor, defectLabel, GROUP_COLOR, issueUrlFor } from './defects';
import { launchOptionLabel } from './launches';

const line = { resultId: 'r', attempt: 1, seq: 1, ts: '2026-11-21T10:00:00Z', level: 'info', stream: 'stdout', message: 'hi' };

describe('logs', () => {
  it('parses batched, wrapped and single log events', () => {
    expect(parseLogEvent(JSON.stringify([line, line]))).toHaveLength(2);
    expect(parseLogEvent(JSON.stringify({ lines: [line] }))).toHaveLength(1);
    expect(parseLogEvent(JSON.stringify({ items: [line] }))).toHaveLength(1);
    expect(parseLogEvent(JSON.stringify(line))).toHaveLength(1);
    expect(parseLogEvent('not json')).toEqual([]);
    expect(parseLogEvent(JSON.stringify([{ nope: 1 }]))).toEqual([]);
  });

  it('normalizes levels and streams', () => {
    expect(toLogLine({ ...line, level: 'WARNING', stream: 'weird' })).toMatchObject({ level: 'warn', stream: 'stdout' });
    expect(toLogLine({ ...line, level: 'fatal' })?.level).toBe('error');
    expect(toLogLine({ ...line, level: 'chatty' })?.level).toBe('info');
  });

  it('reads gap sizes', () => {
    expect(parseGapEvent('{"skipped":12}')).toBe(12);
    expect(parseGapEvent('{"dropped":3}')).toBe(3);
    expect(parseGapEvent('x')).toBe(0);
  });

  it('caps the buffer and counts dropped lines', () => {
    const e = (n: number): LogEntry => ({ kind: 'gap', key: String(n), skipped: n });
    const b = appendEntries({ entries: [e(1), e(2)], trimmed: 0 }, [e(3), e(4), e(5)], 3);
    expect(b.entries.map((x) => x.key)).toEqual(['3', '4', '5']);
    expect(b.trimmed).toBe(2);
  });
});

describe('defects', () => {
  it('builds issue links only for keys that match the tracker', () => {
    expect(issueUrlFor('https://issues.example.com/browse/{key}', '^[A-Z]+-\\d+$', 'SHOP-1')).toBe('https://issues.example.com/browse/SHOP-1');
    expect(issueUrlFor('https://issues.example.com/browse/{key}', '^[A-Z]+-\\d+$', 'shop')).toBeNull();
    expect(issueUrlFor('https://github.com/example/r/issues/{key}', '^#?\\d+$', '#42')).toBe('https://github.com/example/r/issues/42');
    expect(issueUrlFor('javascript:{key}', '', 'x')).toBeNull();
    expect(issueUrlFor('https://x.example.com/{key}', '(', 'x')).toBeNull();
  });

  it('labels and colors a defect from its type, group or stage B name', () => {
    const t1 = { id: 't1', group: 'productBug' as const, name: 'Checkout bug', shortName: 'CB', color: '#123456', builtIn: false };
    const types = new Map([['t1', t1]]);
    expect(defectLabel({ typeId: 't1' }, types)).toBe('Checkout bug');
    expect(defectColor({ typeId: 't1' }, types)).toBe('#123456');
    expect(defectLabel({ group: 'systemIssue' })).toBe('System issue');
    expect(defectColor({ group: 'systemIssue' })).toBe(GROUP_COLOR.systemIssue);
    expect(defectLabel({ type: 'automation_bug' })).toBe('automation bug');
    expect(defectColor({ typeId: 'x', group: null }, new Map([['x', { ...t1, id: 'x', color: 'red;background:url(x)' }]]))).toBe(GROUP_COLOR.toInvestigate);
  });
});

describe('settings and labels', () => {
  it('fills settings defaults', () => {
    const s = withSettingsDefaults({ flaky: { historyDepth: 20, stabilizationPeriod: 5, includePassedTests: true } });
    expect(s.flaky.historyDepth).toBe(20);
    expect(s.retention.rawDays).toBe(90);
    expect(s.gates).toEqual([]);
  });

  it('does not repeat the branch already in the launch name', () => {
    const base = { id: 'l', number: 3, startedAt: new Date().toISOString() } as Parameters<typeof launchOptionLabel>[0];
    expect(launchOptionLabel({ ...base, name: 'shop fix/x', vcs: { branch: 'fix/x' } })).not.toContain('· fix/x');
    expect(launchOptionLabel({ ...base, name: 'nightly', vcs: { branch: 'main' } })).toContain('· main');
  });
});

describe('csp nonce', () => {
  it('reads the server nonce and ignores the placeholder', () => {
    const meta = document.createElement('meta');
    meta.name = 'csp-nonce';
    meta.content = '__CSP_NONCE__';
    document.head.appendChild(meta);
    resetCspNonceCache();
    expect(cspNonce()).toBeUndefined();
    meta.content = 'LBAvKkc8vV1pLPUdT7XEYxSQ';
    resetCspNonceCache();
    expect(cspNonce()).toBe('LBAvKkc8vV1pLPUdT7XEYxSQ');
    meta.content = '"><script>';
    resetCspNonceCache();
    expect(cspNonce()).toBeUndefined();
    meta.remove();
    resetCspNonceCache();
  });
});
