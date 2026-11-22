import type { AttachmentRef, CaseStepHtml, RunItem, RunStatus, StepStatus } from '../../api/tm';

/** A step result as the runner shows it (applied at once, saved in the background). */
export interface LocalStep {
  status: StepStatus;
  actual: string;
  comment: string;
  attachments: AttachmentRef[];
}

export function emptyLocal(): LocalStep {
  return { status: 'untested', actual: '', comment: '', attachments: [] };
}

export function isOpen(s: RunStatus): boolean {
  return s === 'untested' || s === 'in_progress';
}

export function htmlFor(item: RunItem, path: string): CaseStepHtml | undefined {
  let list = item.stepsHtml;
  let found: CaseStepHtml | undefined;
  for (const part of path.split('.')) {
    found = list[Number(part) - 1];
    if (!found) return undefined;
    list = found.steps;
  }
  return found;
}
