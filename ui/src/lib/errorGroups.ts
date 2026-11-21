import type { ResultSummary } from '../api/types';

export interface FailureGroup {
  key: string;
  title: string;
  by: 'message' | 'fingerprint' | 'none';
  results: ResultSummary[];
}

const PREFIX = 96;

export function messagePrefix(message: string): string {
  const firstLine = message.split('\n').find((l) => l.trim() !== '') ?? '';
  const normalized = firstLine
    .replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi, '<uuid>')
    .replace(/\b0x[0-9a-f]+\b/gi, '<hex>')
    .replace(/\b[0-9a-f]{12,}\b/gi, '<hex>')
    .replace(/(?<![A-Za-z_])\d+(\.\d+)?/g, '<n>')
    .replace(/\s+/g, ' ')
    .trim();
  return normalized.length > PREFIX ? `${normalized.slice(0, PREFIX)}…` : normalized;
}

export function isProblem(r: ResultSummary): boolean {
  return r.status === 'failed' || r.status === 'broken';
}

export function groupFailures(results: readonly ResultSummary[]): FailureGroup[] {
  const groups = new Map<string, FailureGroup>();
  for (const r of results) {
    if (!isProblem(r)) continue;
    let key: string;
    let title: string;
    let by: FailureGroup['by'];
    if (r.message && r.message.trim()) {
      title = messagePrefix(r.message);
      key = `m:${title}`;
      by = 'message';
    } else if (r.fingerprint) {
      key = `f:${r.fingerprint}`;
      title = `Fingerprint ${r.fingerprint.slice(0, 12)}`;
      by = 'fingerprint';
    } else {
      key = 'none';
      title = 'No error message';
      by = 'none';
    }
    const g = groups.get(key);
    if (g) g.results.push(r);
    else groups.set(key, { key, title, by, results: [r] });
  }
  return [...groups.values()].sort((a, b) => b.results.length - a.results.length || a.title.localeCompare(b.title));
}
