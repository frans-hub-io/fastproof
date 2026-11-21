import type { TestCaseHistoryItem } from '../api/types';

export function historyStats(items: readonly TestCaseHistoryItem[]) {
  const executed = items.filter((i) => i.status !== 'skipped');
  const passed = executed.filter((i) => i.status === 'passed').length;
  let flips = 0;
  for (let i = 1; i < executed.length; i++) {
    const a = executed[i - 1];
    const b = executed[i];
    if (a && b && (a.status === 'passed') !== (b.status === 'passed')) flips++;
  }
  const durations = executed.map((i) => i.durationMs).filter((d): d is number => d !== null);
  const avg = durations.length ? durations.reduce((s, d) => s + d, 0) / durations.length : null;
  const lastFailure = items.find((i) => i.status === 'failed' || i.status === 'broken') ?? null;
  return { runs: items.length, passRate: executed.length ? passed / executed.length : 0, flipRate: executed.length > 1 ? flips / (executed.length - 1) : 0, avg, lastFailure, executed: executed.length };
}
