import type { GateMetric, GateOp, GateRule } from '../api/types-next';

export const METRICS: { value: GateMetric; label: string; unit: string; hint: string; analysis?: boolean }[] = [
  { value: 'failed', label: 'Failed results', unit: 'results', hint: 'Number of failed results, muted ones left out' },
  { value: 'broken', label: 'Broken results', unit: 'results', hint: 'Number of broken results, muted ones left out' },
  { value: 'newFailures', label: 'New failures', unit: 'results', hint: 'Failed or broken results that were not failing in the previous launch' },
  { value: 'passRate', label: 'Pass rate', unit: '%', hint: 'Percent of executed (non-skipped) results' },
  { value: 'flaky', label: 'Flaky results', unit: 'results', hint: 'Number of results that passed on a retry' },
  { value: 'duration', label: 'Duration', unit: 'seconds', hint: 'Launch duration in seconds' },
  { value: 'newErrors', label: 'New unique errors', unit: 'errors', hint: 'Unique errors of the launch not seen on its branch in the last 30 days, ignored ones left out (Allure: new defects)', analysis: true },
  { value: 'toInvestigate', label: 'To investigate', unit: 'results', hint: 'Failed or broken results, not muted, without a defect or with a to-investigate one (ReportPortal: To investigate)', analysis: true },
  { value: 'total', label: 'Total results', unit: 'results', hint: 'Results that ran: every status except skipped (ReportPortal: Total; Allure: total)', analysis: true },
];

export const OPS: { value: GateOp; label: string }[] = [
  { value: 'lte', label: '≤' },
  { value: 'lt', label: '<' },
  { value: 'gte', label: '≥' },
  { value: 'gt', label: '>' },
];

const OP_LABEL: Record<GateOp, string> = { gt: '>', gte: '≥', lt: '<', lte: '≤' };

export function metricOf(m: GateMetric) {
  return METRICS.find((x) => x.value === m) ?? { value: m, label: m, unit: '', hint: '' };
}

function unitOf(m: GateMetric): string {
  const u = metricOf(m).unit;
  return u === '%' ? '%' : u === 'seconds' ? ' s' : '';
}

export function ruleText(r: GateRule): string {
  return `${metricOf(r.metric).label} ${OP_LABEL[r.op]} ${String(r.value)}${unitOf(r.metric)}${r.baseline ? ` (vs ${r.baseline})` : ''}`;
}

/** A measured value with the unit of its metric; a dash when it could not be measured. */
export function actualText(metric: GateMetric, actual: number | null | undefined): string {
  if (actual === null || actual === undefined) return '—';
  const v = Number.isInteger(actual) ? String(actual) : actual.toFixed(2);
  return `${v}${unitOf(metric)}`;
}

export const GLOB = /^[A-Za-z0-9._/*-]{1,200}$/;

export const MAX_RULES = 20;

export function ruleError(r: GateRule): string | null {
  if (!Number.isFinite(r.value)) return 'Value is required';
  if (r.baseline && !GLOB.test(r.baseline)) return 'Baseline must be a branch name';
  if (r.baseline) return r.metric === 'passRate' && Math.abs(r.value) > 100 ? 'The difference in pass rate is -100 to 100' : null;
  if (r.metric === 'passRate' && (r.value < 0 || r.value > 100)) return 'Pass rate is 0–100';
  if (r.value < 0) return 'Value cannot be negative';
  return null;
}

export function rulesValid(rules: GateRule[]): boolean {
  return rules.length > 0 && rules.length <= MAX_RULES && rules.every((r) => ruleError(r) === null);
}

/** The metrics a rule editor offers: the analysis ones only where Analysis answers, or when a rule already uses them. */
export function availableMetrics(analysis: boolean, rules: GateRule[]): GateMetric[] {
  return METRICS.filter((m) => !m.analysis || analysis || rules.some((r) => r.metric === m.value)).map((m) => m.value);
}

/** What a rule measures, as the editor explains it under the row. */
export function ruleHint(r: GateRule): string {
  const m = metricOf(r.metric);
  if (!r.baseline) return m.hint;
  if (r.metric === 'newErrors') return `Errors that are new compared with the history of ${r.baseline}`;
  if (r.metric === 'newFailures') return `Compared with the last finished launch on ${r.baseline}`;
  return `Difference to the last finished launch on ${r.baseline}`;
}
