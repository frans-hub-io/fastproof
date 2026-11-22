import type { CaseStep, RunStatus, RunStepResult, StepStatus } from '../../api/tm';

export const MAX_STEP_DEPTH = 3;

export interface FlatStep {
  /** `1`, `1.2`, `1.2.3`: 1-based positions, the run item's step path. */
  path: string;
  step: CaseStep;
  depth: number;
  parent: string | null;
  hasChildren: boolean;
}

export function flattenSteps(steps: readonly CaseStep[], prefix = '', depth = 0, out: FlatStep[] = []): FlatStep[] {
  steps.forEach((s, i) => {
    const path = prefix ? `${prefix}.${String(i + 1)}` : String(i + 1);
    out.push({ path, step: s, depth, parent: prefix || null, hasChildren: s.steps.length > 0 });
    flattenSteps(s.steps, path, depth + 1, out);
  });
  return out;
}

export function countSteps(steps: readonly CaseStep[]): number {
  return steps.reduce((n, s) => n + 1 + countSteps(s.steps), 0);
}

function indexes(path: string): number[] {
  return path.split('.').map((p) => Number(p) - 1);
}

export function pathOf(idx: readonly number[]): string {
  return idx.map((i) => String(i + 1)).join('.');
}

export function stepAt(steps: readonly CaseStep[], path: string): CaseStep | undefined {
  let list = steps;
  let found: CaseStep | undefined;
  for (const i of indexes(path)) {
    found = list[i];
    if (!found) return undefined;
    list = found.steps;
  }
  return found;
}

export function newStepId(): string {
  const raw = typeof crypto !== 'undefined' && 'randomUUID' in crypto ? crypto.randomUUID().replace(/-/g, '') : Math.random().toString(16).slice(2) + Date.now().toString(16);
  return `s_${raw.slice(0, 20)}`;
}

export function emptyStep(): CaseStep {
  return { id: newStepId(), action: '', expected: '', data: '', attachments: [], steps: [] };
}

/** Returns a copy of `steps` where the sibling list holding `path` was replaced by `fn(list, index)`. */
function editSiblings(steps: readonly CaseStep[], path: string, fn: (list: CaseStep[], index: number) => CaseStep[]): CaseStep[] {
  const idx = indexes(path);
  const walk = (list: readonly CaseStep[], level: number): CaseStep[] => {
    const i = idx[level] ?? 0;
    if (level === idx.length - 1) return fn([...list], i);
    return list.map((s, j) => (j === i ? { ...s, steps: walk(s.steps, level + 1) } : s));
  };
  return walk(steps, 0);
}

export function updateStep(steps: readonly CaseStep[], path: string, patch: Partial<Omit<CaseStep, 'id' | 'steps'>>): CaseStep[] {
  return editSiblings(steps, path, (list, i) => list.map((s, j) => (j === i ? { ...s, ...patch } : s)));
}

/** Inserts `step` right after `path` (or at the end when `path` is null); returns the new list and the new path. */
export function insertAfter(steps: readonly CaseStep[], path: string | null, step: CaseStep = emptyStep()): { steps: CaseStep[]; path: string } {
  if (!path) return { steps: [...steps, step], path: String(steps.length + 1) };
  const idx = indexes(path);
  const last = idx[idx.length - 1] ?? 0;
  const next = editSiblings(steps, path, (list, i) => [...list.slice(0, i + 1), step, ...list.slice(i + 1)]);
  return { steps: next, path: pathOf([...idx.slice(0, -1), last + 1]) };
}

export function removeStep(steps: readonly CaseStep[], path: string): CaseStep[] {
  return editSiblings(steps, path, (list, i) => list.filter((_, j) => j !== i));
}

/** Moves a step among its siblings; returns null when it cannot move that way. */
export function moveStep(steps: readonly CaseStep[], path: string, dir: -1 | 1): { steps: CaseStep[]; path: string } | null {
  const idx = indexes(path);
  const i = idx[idx.length - 1] ?? 0;
  const parent = idx.length > 1 ? stepAt(steps, pathOf(idx.slice(0, -1))) : null;
  const siblings = parent ? parent.steps : steps;
  const j = i + dir;
  if (j < 0 || j >= siblings.length) return null;
  const next = editSiblings(steps, path, (list) => {
    const out = [...list];
    const a = out[i];
    const b = out[j];
    if (!a || !b) return list;
    out[i] = b;
    out[j] = a;
    return out;
  });
  return { steps: next, path: pathOf([...idx.slice(0, -1), j]) };
}

function subtreeDepth(s: CaseStep): number {
  return 1 + Math.max(0, ...s.steps.map(subtreeDepth));
}

/** Makes the step the last child of its previous sibling (Tab); null when not possible. */
export function indentStep(steps: readonly CaseStep[], path: string, maxDepth = MAX_STEP_DEPTH): { steps: CaseStep[]; path: string } | null {
  const idx = indexes(path);
  const i = idx[idx.length - 1] ?? 0;
  if (i === 0) return null;
  const step = stepAt(steps, path);
  if (!step) return null;
  if (idx.length + subtreeDepth(step) > maxDepth) return null;
  const prevPath = pathOf([...idx.slice(0, -1), i - 1]);
  const prev = stepAt(steps, prevPath);
  if (!prev) return null;
  const without = removeStep(steps, path);
  const next = editSiblings(without, prevPath, (list, k) => list.map((s, j) => (j === k ? { ...s, steps: [...s.steps, step] } : s)));
  return { steps: next, path: pathOf([...idx.slice(0, -1), i - 1, prev.steps.length]) };
}

/** Moves the step out of its parent, right after it (Shift+Tab); null at the top level. */
export function outdentStep(steps: readonly CaseStep[], path: string): { steps: CaseStep[]; path: string } | null {
  const idx = indexes(path);
  if (idx.length < 2) return null;
  const step = stepAt(steps, path);
  if (!step) return null;
  const parentIdx = idx.slice(0, -1);
  const parentPath = pathOf(parentIdx);
  const without = removeStep(steps, path);
  const next = editSiblings(without, parentPath, (list, k) => [...list.slice(0, k + 1), step, ...list.slice(k + 1)]);
  const last = parentIdx[parentIdx.length - 1] ?? 0;
  return { steps: next, path: pathOf([...parentIdx.slice(0, -1), last + 1]) };
}

/** Severity used for "worst child": failed > blocked > skipped > passed; untested counts only when alone. */
const SEVERITY: Record<StepStatus, number> = { untested: 0, passed: 1, skipped: 2, blocked: 3, failed: 4 };

export function worstStatus(list: readonly StepStatus[]): StepStatus {
  let worst: StepStatus = 'untested';
  for (const s of list) if (SEVERITY[s] > SEVERITY[worst]) worst = s;
  return worst;
}

export type StepResultMap = ReadonlyMap<string, StepStatus>;

export function resultMap(results: readonly Pick<RunStepResult, 'path' | 'status'>[]): Map<string, StepStatus> {
  return new Map(results.map((r) => [r.path, r.status] as const));
}

/**
 * Effective status of the step at `path`: a recorded status wins (a parent can be overridden), otherwise a parent
 * takes the worst status of its children, ignoring untested ones while others exist.
 */
export function effectiveStatus(step: CaseStep, path: string, results: StepResultMap): StepStatus {
  const own = results.get(path);
  if (own && own !== 'untested') return own;
  if (!step.steps.length) return own ?? 'untested';
  const children = step.steps.map((c, i) => effectiveStatus(c, `${path}.${String(i + 1)}`, results));
  return worstStatus(children);
}

export function topLevelStatuses(steps: readonly CaseStep[], results: StepResultMap): StepStatus[] {
  return steps.map((s, i) => effectiveStatus(s, String(i + 1), results));
}

/** Every step path with its effective status, in display order. */
export function effectiveStatuses(steps: readonly CaseStep[], results: StepResultMap): Map<string, StepStatus> {
  const out = new Map<string, StepStatus>();
  for (const f of flattenSteps(steps)) out.set(f.path, effectiveStatus(f.step, f.path, results));
  return out;
}

/**
 * Final status suggested for a case: any failed → failed, any blocked → blocked, untested left → in progress
 * (unless they become skipped), all skipped → skipped, otherwise passed. A case without steps has no suggestion.
 */
export function suggestedStatus(steps: readonly CaseStep[], results: StepResultMap, untestedToSkipped = false): RunStatus {
  if (!steps.length) return 'untested';
  const top = topLevelStatuses(steps, results).map((s) => (s === 'untested' && untestedToSkipped ? 'skipped' : s));
  if (top.includes('failed')) return 'failed';
  if (top.includes('blocked')) return 'blocked';
  if (top.includes('untested')) return top.every((s) => s === 'untested') ? 'untested' : 'in_progress';
  if (top.every((s) => s === 'skipped')) return 'skipped';
  return 'passed';
}

/** Item status the server keeps while steps are being recorded. */
export function itemStatusFromSteps(results: StepResultMap): RunStatus {
  return [...results.values()].some((s) => s !== 'untested') ? 'in_progress' : 'untested';
}

/** Leaf paths (steps without children) that have no recorded status. */
export function untestedLeaves(steps: readonly CaseStep[], results: StepResultMap): string[] {
  return flattenSteps(steps)
    .filter((f) => !f.hasChildren && (results.get(f.path) ?? 'untested') === 'untested')
    .map((f) => f.path);
}

export const RUN_STATUS_LABEL: Record<RunStatus, string> = {
  untested: 'Untested',
  in_progress: 'In progress',
  passed: 'Passed',
  failed: 'Failed',
  blocked: 'Blocked',
  skipped: 'Skipped',
};
