import { loadTestPlan, type TestPlan } from '@franshub/fastproof-playwright';

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * A `testNamePattern` for `jest.config` that keeps the tests named by the plan's selectors (`file#describe title`
 * or `file › describe › title`) and ids written as `[fastproof-id:ID]` in the title. `undefined` without a plan.
 * The reporter additionally leaves out of the launch every test the plan does not select.
 */
export function testPlanPattern(plan: TestPlan | undefined = loadTestPlan()): string | undefined {
  if (!plan) return undefined;
  const parts: string[] = [];
  for (const t of plan.tests) {
    if (t.id !== undefined && t.id !== null && String(t.id)) parts.push(`\\[(?:fastproof-id|allure-id):${escapeRegExp(String(t.id))}\\]|@allure\\.id[=:]${escapeRegExp(String(t.id))}(?:\\s|$)`);
    const sel = t.selector?.trim();
    if (!sel) continue;
    if (sel.includes(' › ')) parts.push(`^${escapeRegExp(sel.split(' › ').slice(1).join(' '))}$`);
    else if (sel.includes('#')) parts.push(`^${escapeRegExp(sel.slice(sel.indexOf('#') + 1))}$`);
  }
  return parts.length ? parts.join('|') : '(?!)';
}
