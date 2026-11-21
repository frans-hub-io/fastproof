import type { GateMetric, GateOp, GateRule } from '../api/types-next';

export const METRICS: { value: GateMetric; label: string; unit: string }[] = [
  { value: 'failed', label: 'Failed results', unit: 'results' },
  { value: 'broken', label: 'Broken results', unit: 'results' },
  { value: 'newFailures', label: 'New failures', unit: 'results' },
  { value: 'passRate', label: 'Pass rate', unit: '%' },
  { value: 'flaky', label: 'Flaky results', unit: 'results' },
  { value: 'duration', label: 'Duration', unit: 'seconds' },
];

export const OPS: { value: GateOp; label: string }[] = [
  { value: 'lte', label: '≤' },
  { value: 'lt', label: '<' },
  { value: 'gte', label: '≥' },
  { value: 'gt', label: '>' },
];

const OP_LABEL: Record<GateOp, string> = { gt: '>', gte: '≥', lt: '<', lte: '≤' };

export function metricOf(m: GateMetric) {
  return METRICS.find((x) => x.value === m) ?? { value: m, label: m, unit: '' };
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
