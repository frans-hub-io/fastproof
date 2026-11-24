import { inTestPlan, loadTestPlan } from '@franshub/fastproof-playwright';
import { beforeEach } from 'vitest';

type Named = { name: string; suite?: Named; type?: string };

/**
 * Setup file for `test.setupFiles`: with a test plan (`FASTPROOF_TESTPLAN` or `ALLURE_TESTPLAN_PATH`) it skips
 * the tests the plan does not name; the reporter leaves those out of the launch. Without a plan it does nothing.
 */
const plan = loadTestPlan();
if (plan) {
  beforeEach((ctx) => {
    const task = ctx.task as unknown as Named & {
      file?: { name: string };
      location?: { line: number; column: number };
      meta?: { fastproofId?: unknown };
    };
    const describes: string[] = [];
    for (let s = task.suite; s && s.type === 'suite'; s = s.suite) describes.unshift(s.name);
    const id = task.meta?.fastproofId;
    const ok = inTestPlan(plan, {
      titlePath: [task.file?.name ?? '', ...describes.filter(Boolean), task.name],
      line: task.location?.line,
      column: task.location?.column,
      annotations: typeof id === 'string' && id ? [{ type: 'fastproof.id', description: id }] : [],
    });
    if (!ok) ctx.skip('not in the test plan');
  });
}
