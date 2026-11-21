import { describe, expect, it } from 'vitest';
import type { Attachment, ResultSummary, TestCaseHistoryItem } from '../api/types';
import { hasAnsi, parseAnsi, stripAnsi } from './ansi';
import { groupAttachments, isActiveContent } from './attachments';
import { commitUrl, repoLabel } from './ci';
import { diffLines } from './diff';
import { groupFailures, messagePrefix } from './errorGroups';
import { formatBytes, formatDuration, formatRelative, isHttpUrl, percent } from './format';
import { historyStats } from './history';
import { safeDataImage } from './images';
import { buildCreateBody } from './tokens';

const ESC = '\u001b';

describe('ansi', () => {
  it('parses colors, bold and resets', () => {
    const s = parseAnsi(`plain ${ESC}[1;31mred bold${ESC}[0m end`);
    expect(s.map((x) => x.text)).toEqual(['plain ', 'red bold', ' end']);
    expect(s[1]?.style).toEqual({ bold: true, fg: 'var(--fp-ansi-1)' });
    expect(s[2]?.style).toEqual({});
  });
  it('supports 256 and true colors and bright backgrounds', () => {
    const s = parseAnsi(`${ESC}[38;5;208ma${ESC}[38;2;1;2;3mb${ESC}[101mc`);
    expect(s[0]?.style.fg).toBe('rgb(255, 135, 0)');
    expect(s[1]?.style.fg).toBe('rgb(1, 2, 3)');
    expect(s[2]?.style.bg).toBe('var(--fp-ansi-9)');
  });
  it('drops cursor and OSC sequences', () => {
    expect(stripAnsi(`a${ESC}[2Kb${ESC}]0;title\u0007c`)).toBe('abc');
    expect(hasAnsi('abc')).toBe(false);
  });
});

describe('diff', () => {
  it('finds added and removed lines', () => {
    const d = diffLines('a\nb\nc', 'a\nx\nc');
    expect(d).toEqual([
      { op: 'equal', text: 'a' },
      { op: 'remove', text: 'b' },
      { op: 'add', text: 'x' },
      { op: 'equal', text: 'c' },
    ]);
  });
  it('handles identical input', () => {
    expect(diffLines('same', 'same')).toEqual([{ op: 'equal', text: 'same' }]);
  });
});

function result(id: string, status: ResultSummary['status'], message?: string, fingerprint: string | null = null): ResultSummary {
  return { id, testCaseId: id, seriesKey: id, name: id, fullName: id, suitePath: [], status, flakyInRun: false, attemptCount: 1, durationMs: 1, labels: [], muted: false, fingerprint, message };
}

describe('failure groups', () => {
  it('normalizes numbers, ids and keeps word-internal digits', () => {
    expect(messagePrefix('Timeout 30000ms exceeded for 4f1c2a8e-63b0-4bd2-9d8e-0c5cfbd1a5e1\nmore')).toBe('Timeout <n>ms exceeded for <uuid>');
    expect(messagePrefix('org.opentest4j.AssertionFailedError: expected: <201>')).toBe('org.opentest4j.AssertionFailedError: expected: <<n>>');
  });
  it('groups by message, then fingerprint, largest first', () => {
    const groups = groupFailures([
      result('1', 'failed', 'Timeout 30000ms'),
      result('2', 'broken', 'Timeout 35000ms'),
      result('3', 'failed', undefined, 'abc'),
      result('4', 'passed', 'ignored'),
      result('5', 'failed'),
    ]);
    expect(groups.map((g) => [g.by, g.results.length])).toEqual([
      ['message', 2],
      ['fingerprint', 1],
      ['none', 1],
    ]);
  });
});

describe('format', () => {
  it('formats durations and sizes', () => {
    expect(formatDuration(450)).toBe('450 ms');
    expect(formatDuration(7060)).toBe('7.06 s');
    expect(formatDuration(125_000)).toBe('2m 5s');
    expect(formatDuration(null)).toBe('—');
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(1536)).toBe('1.5 KB');
    expect(percent(1, 3)).toBe('33.3%');
  });
  it('formats relative times', () => {
    const now = Date.parse('2026-11-21T12:00:00Z');
    expect(formatRelative('2026-11-21T11:58:00Z', now)).toBe('2 min ago');
    expect(formatRelative('2026-11-21T09:00:00Z', now)).toBe('3 h ago');
    expect(formatRelative(null, now)).toBe('never');
  });
  it('accepts only http(s) URLs', () => {
    expect(isHttpUrl('https://example.com')).toBe(true);
    expect(isHttpUrl('javascript:alert(1)')).toBe(false);
    expect(isHttpUrl('data:text/html,x')).toBe(false);
  });
});

function att(name: string, contentType: string, kind: Attachment['kind'] = 'image'): Attachment {
  return { id: name, name, contentType, kind, size: 10 };
}

describe('attachments', () => {
  it('groups Playwright expected/actual/diff triples', () => {
    const e = groupAttachments([att('shot-expected.png', 'image/png'), att('a.log', 'text/plain', 'log'), att('shot-actual.png', 'image/png'), att('shot-diff.png', 'image/png')]);
    expect(e).toHaveLength(2);
    const first = e[0];
    expect(first?.kind).toBe('playwrightDiff');
    if (first?.kind === 'playwrightDiff') {
      expect(first.name).toBe('shot');
      expect(first.diff?.name).toBe('shot-diff.png');
    }
  });
  it('keeps a lone expected image as a single attachment', () => {
    expect(groupAttachments([att('x-expected.png', 'image/png')])[0]?.kind).toBe('single');
  });
  it('treats html, svg and xml as active content', () => {
    expect(isActiveContent(att('r.html', 'text/html', 'other'))).toBe(true);
    expect(isActiveContent(att('b.svg', 'image/svg+xml'))).toBe(true);
    expect(isActiveContent(att('report', 'application/xml; charset=utf-8', 'other'))).toBe(true);
    expect(isActiveContent(att('p.png', 'image/png'))).toBe(false);
  });
  it('accepts only raster data URIs for image diffs', () => {
    expect(safeDataImage('data:image/png;base64,AAAA')).toBe('data:image/png;base64,AAAA');
    expect(safeDataImage('data:image/svg+xml;base64,AAAA')).toBeUndefined();
    expect(safeDataImage('https://example.com/x.png')).toBeUndefined();
  });
});

describe('ci links', () => {
  it('builds commit links for GitLab and GitHub', () => {
    expect(commitUrl({ repository: 'https://gitlab.example.com/shop/web', branch: 'main', commit: 'abc' }, { provider: 'gitlab', pipelineId: null, jobId: null, url: null, buildOrder: null })).toBe(
      'https://gitlab.example.com/shop/web/-/commit/abc',
    );
    expect(commitUrl({ repository: 'https://github.com/example/app.git', branch: null, commit: 'abc' }, null)).toBe('https://github.com/example/app/commit/abc');
    expect(commitUrl({ repository: 'git@example.com:x.git', branch: null, commit: 'abc' }, null)).toBeNull();
    expect(repoLabel('https://github.com/example/app.git')).toBe('example/app');
  });
});

describe('token body', () => {
  it('maps the scope choice', () => {
    const base = { name: ' ci ', role: 'uploader', scopes: ['web-shop'], days: 30, description: '' };
    expect(buildCreateBody({ ...base, mode: 'scopes' })).toEqual({ name: 'ci', role: 'uploader', scopes: ['web-shop'], expiresInDays: 30 });
    expect(buildCreateBody({ ...base, mode: 'all' }).scopes).toEqual(['*']);
    expect(buildCreateBody({ ...base, mode: 'global', days: '' })).toEqual({ name: 'ci', role: 'uploader', global: true });
  });
});

describe('history stats', () => {
  it('computes pass rate and flips over executed runs', () => {
    const h = (status: TestCaseHistoryItem['status'], durationMs: number | null = 100): TestCaseHistoryItem => ({
      launchId: 'l',
      launchNumber: 1,
      resultId: status + String(durationMs),
      status,
      startedAt: '2026-11-21T00:00:00Z',
      durationMs,
      branch: 'main',
      environment: null,
    });
    const s = historyStats([h('passed'), h('failed', 300), h('skipped', null), h('passed', 200)]);
    expect(s.runs).toBe(4);
    expect(s.passRate).toBeCloseTo(2 / 3);
    expect(s.flipRate).toBe(1);
    expect(s.avg).toBe(200);
    expect(s.lastFailure?.status).toBe('failed');
  });
});
