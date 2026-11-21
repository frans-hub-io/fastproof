import { readFileSync } from 'node:fs';

/** Allure-format test plan: `{version: "1.0", tests: [{id, selector}]}`. */
export interface TestPlan {
  version?: string;
  tests: { id?: string | number; selector?: string }[];
}

/** Annotation types whose description is an explicit test case id. */
export const ID_ANNOTATIONS = ['fastproof.id', 'allure_id', 'as_id'];
export const FULL_NAME_SEPARATOR = ' › ';

type Env = Record<string, string | undefined>;

/** Reads the plan from `path`, `FASTPROOF_TESTPLAN` or `ALLURE_TESTPLAN_PATH`; `undefined` when none is set. */
export function loadTestPlan(path?: string, env: Env = process.env): TestPlan | undefined {
  const p = path ?? env.FASTPROOF_TESTPLAN ?? env.ALLURE_TESTPLAN_PATH;
  if (!p) return undefined;
  try {
    const plan = JSON.parse(readFileSync(p, 'utf8')) as TestPlan;
    if (!plan || !Array.isArray(plan.tests)) return undefined;
    return plan;
  } catch {
    return undefined;
  }
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** Selector forms: `file › describe › title` (FastProof), `file#describe title` (Allure) and `file:line[:col]`. */
function selectorTitles(selector: string): string | undefined {
  const s = selector.trim();
  if (s.includes(FULL_NAME_SEPARATOR)) return s.split(FULL_NAME_SEPARATOR).join(' ');
  const hash = s.indexOf('#');
  if (hash > 0) return `${s.slice(0, hash)} ${s.slice(hash + 1)}`;
  return undefined;
}

/**
 * A `grep` for `playwright.config` that keeps only the tests named by the plan's selectors, or `undefined`
 * without a plan. Plans with ids or `file:line` selectors need the `fastproofTestPlan` fixture instead.
 */
export function testPlanGrep(plan: TestPlan | undefined = loadTestPlan()): RegExp | undefined {
  if (!plan) return undefined;
  const parts = plan.tests
    .map((t) => (t.selector ? selectorTitles(t.selector) : undefined))
    .filter((s): s is string => Boolean(s))
    .map((s) => `(?:^| )${escapeRegExp(s)}(?: @|$)`);
  if (!parts.length) return /$^/;
  return new RegExp(parts.join('|'));
}

export interface PlanTestInfo {
  /** `[file, ...describes, title]` with the file relative to the test directory. */
  titlePath: string[];
  line?: number;
  column?: number;
  annotations: { type: string; description?: string }[];
}

export function fullNameOf(titlePath: string[]): string {
  return titlePath.filter((t, i) => i === 0 || i === titlePath.length - 1 || t !== '').join(FULL_NAME_SEPARATOR);
}

/** Whether a test is in the plan, by id annotation or by any selector form. */
export function inTestPlan(plan: TestPlan, t: PlanTestInfo): boolean {
  const ids = new Set(
    t.annotations.filter((a) => ID_ANNOTATIONS.includes(a.type) && a.description).map((a) => String(a.description)),
  );
  const [file = '', ...titles] = t.titlePath;
  const names = new Set([
    fullNameOf(t.titlePath),
    `${file}#${titles.filter(Boolean).join(' ')}`,
    `${file}:${t.line ?? ''}`,
    `${file}:${t.line ?? ''}:${t.column ?? ''}`,
  ]);
  return plan.tests.some(
    (x) => (x.id !== undefined && x.id !== null && ids.has(String(x.id))) || (x.selector !== undefined && names.has(x.selector.trim())),
  );
}
