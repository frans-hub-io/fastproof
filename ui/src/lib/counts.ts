import type { Counts } from '../api/types';
import { STATUS_ORDER } from './status';

export function countsLabel(c: Counts): string {
  return STATUS_ORDER.filter((s) => c[s] > 0)
    .map((s) => `${String(c[s])} ${s}`)
    .join(', ');
}

export function passRate(c: Counts): number {
  const executed = c.total - c.skipped;
  return executed > 0 ? c.passed / executed : 0;
}
