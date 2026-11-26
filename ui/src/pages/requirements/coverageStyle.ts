import type { CoverageCounts, CoverageStatus } from '../../api/requirements';

/** One set of coverage colours for every page and the dashboard widget. */
export const COVERAGE_COLOR: Record<CoverageStatus, string> = {
  uncovered: 'var(--fp-status-skipped)',
  failed: 'var(--fp-status-failed)',
  notRun: 'var(--fp-status-unknown)',
  partial: 'var(--fp-status-broken)',
  passed: 'var(--fp-status-passed)',
};

export const COVERAGE_LABEL: Record<CoverageStatus, string> = {
  uncovered: 'Uncovered',
  failed: 'Failing',
  notRun: 'Not run',
  partial: 'Partly passed',
  passed: 'Passed',
};

export const COVERAGE_ORDER: readonly CoverageStatus[] = ['uncovered', 'failed', 'notRun', 'partial', 'passed'];

export const CASE_SEGMENTS = [
  { key: 'passed', label: 'passed', color: 'var(--fp-status-passed)' },
  { key: 'failed', label: 'failed', color: 'var(--fp-status-failed)' },
  { key: 'notRun', label: 'not run', color: 'var(--fp-status-unknown)' },
  { key: 'skipped', label: 'skipped', color: 'var(--fp-status-skipped)' },
] as const;

export function countsLabel(c: CoverageCounts): string {
  if (c.total === 0) return 'No cases';
  return CASE_SEGMENTS.filter((s) => c[s.key] > 0)
    .map((s) => `${String(c[s.key])} ${s.label}`)
    .join(', ');
}

