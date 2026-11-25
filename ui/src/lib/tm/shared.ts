import type { SharedStep, SharedStepBrief } from '../../api/sharedSteps';
import type { CaseStep } from '../../api/tm';

/** The shared step as a case lists it (its steps at that version). */
export function briefOf(s: SharedStep): SharedStepBrief {
  return { id: s.id, number: s.number, name: s.name, version: s.version, archived: s.archivedAt !== null, steps: s.steps, stepsHtml: s.html.steps };
}

export interface SharedDraft {
  name: string;
  description: string;
  steps: CaseStep[];
}

/** Why a draft cannot be saved yet, or `null`. */
export function draftProblem(d: SharedDraft): string | null {
  if (!d.name.trim()) return 'Give the shared step a name.';
  if (d.steps.length === 0) return 'Add at least one step.';
  return null;
}

/**
 * The ids of the chosen top-level steps in case order when they are one run of adjacent steps that are not shared
 * references; `null` otherwise (nothing chosen, a gap, an id that is not a top-level step).
 */
export function adjacentRun(steps: readonly CaseStep[], ids: ReadonlySet<string>): string[] | null {
  const at = steps.map((s, i) => (ids.has(s.id) ? i : -1)).filter((i) => i >= 0);
  if (at.length === 0 || at.length !== ids.size) return null;
  const first = at[0] ?? 0;
  const last = at[at.length - 1] ?? 0;
  if (last - first + 1 !== at.length) return null;
  const run = steps.slice(first, last + 1);
  if (run.some((s) => s.shared)) return null;
  return run.map((s) => s.id);
}
