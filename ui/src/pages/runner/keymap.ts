import type { StepStatus } from '../../api/tm';

export type RunnerAction =
  | { type: 'nextStep' }
  | { type: 'prevStep' }
  | { type: 'status'; status: StepStatus }
  | { type: 'comment' }
  | { type: 'attach' }
  | { type: 'passRest' }
  | { type: 'enter' }
  | { type: 'nextCase' }
  | { type: 'prevCase' }
  | { type: 'defect' }
  | { type: 'finish' }
  | { type: 'help' }
  | { type: 'escape' };

export interface KeyLike {
  key: string;
  shiftKey?: boolean;
  ctrlKey?: boolean;
  metaKey?: boolean;
  altKey?: boolean;
}

export interface KeymapContext {
  /** Character-key shortcuts are on (WCAG 2.1.4 lets the user turn them off). */
  singleKeys: boolean;
  /** Focus is in a text field: only modified shortcuts and Escape apply. */
  typing: boolean;
}

/** Maps a key press of the manual runner to its action (null: not a runner key). Keys follow `40` §4.4. */
export function runnerAction(e: KeyLike, ctx: KeymapContext): RunnerAction | null {
  const mod = !!(e.ctrlKey || e.metaKey);
  if (mod && e.key === 'Enter') return { type: 'finish' };
  if (e.key === 'Escape') return { type: 'escape' };
  if (ctx.typing || mod || e.altKey) return null;
  switch (e.key) {
    case 'ArrowDown':
      return { type: 'nextStep' };
    case 'ArrowUp':
      return { type: 'prevStep' };
    case 'Enter':
      return { type: 'enter' };
    default:
      break;
  }
  if (!ctx.singleKeys) return null;
  switch (e.key) {
    case 'j':
      return { type: 'nextStep' };
    case 'k':
      return { type: 'prevStep' };
    case 'p':
      return { type: 'status', status: 'passed' };
    case 'P':
      return e.shiftKey ? { type: 'passRest' } : null;
    case 'f':
      return { type: 'status', status: 'failed' };
    case 'b':
      return { type: 'status', status: 'blocked' };
    case 's':
      return { type: 'status', status: 'skipped' };
    case 'u':
      return { type: 'status', status: 'untested' };
    case 'c':
      return { type: 'comment' };
    case 'a':
      return { type: 'attach' };
    case 'n':
      return { type: 'nextCase' };
    case 'N':
      return e.shiftKey ? { type: 'prevCase' } : null;
    case 'd':
      return { type: 'defect' };
    case '?':
      return { type: 'help' };
    default:
      return null;
  }
}

export interface HelpRow {
  keys: string[];
  label: string;
  singleKey: boolean;
}

export const RUNNER_HELP: HelpRow[] = [
  { keys: ['j', '↓'], label: 'Next step', singleKey: true },
  { keys: ['k', '↑'], label: 'Previous step', singleKey: true },
  { keys: ['p'], label: 'Passed', singleKey: true },
  { keys: ['f'], label: 'Failed (focus moves to the actual result)', singleKey: true },
  { keys: ['b'], label: 'Blocked', singleKey: true },
  { keys: ['s'], label: 'Skipped', singleKey: true },
  { keys: ['u'], label: 'Reset to untested', singleKey: true },
  { keys: ['c'], label: 'Comment on the step', singleKey: true },
  { keys: ['a'], label: 'Attach a file to the step', singleKey: true },
  { keys: ['Shift+P'], label: 'Pass the remaining steps', singleKey: true },
  { keys: ['Enter'], label: 'Next step', singleKey: false },
  { keys: ['n'], label: 'Next case', singleKey: true },
  { keys: ['Shift+N'], label: 'Previous case', singleKey: true },
  { keys: ['d'], label: 'Quick defect', singleKey: true },
  { keys: ['Ctrl+Enter'], label: 'Finish the case with the suggested status', singleKey: false },
  { keys: ['?'], label: 'This help', singleKey: true },
  { keys: ['Esc'], label: 'Leave a text field', singleKey: false },
];

export type EditorAction = { type: 'save' } | { type: 'newStepBelow' } | { type: 'moveStep'; dir: -1 | 1 } | { type: 'indent' } | { type: 'outdent' } | { type: 'release' };

/**
 * Keys of the case editor. Inside a step field Tab and Shift+Tab change nesting; Escape releases the field so the
 * next Tab moves focus as usual (no keyboard trap).
 */
export function editorAction(e: KeyLike, ctx: { inStep: boolean; released: boolean }): EditorAction | null {
  const mod = !!(e.ctrlKey || e.metaKey);
  if (mod && !e.altKey && (e.key === 's' || e.key === 'S')) return { type: 'save' };
  if (!ctx.inStep) return null;
  if (mod && e.key === 'Enter') return { type: 'newStepBelow' };
  if (e.altKey && !mod && e.key === 'ArrowUp') return { type: 'moveStep', dir: -1 };
  if (e.altKey && !mod && e.key === 'ArrowDown') return { type: 'moveStep', dir: 1 };
  if (e.key === 'Escape') return { type: 'release' };
  if (e.key === 'Tab' && !mod && !e.altKey && !ctx.released) return e.shiftKey ? { type: 'outdent' } : { type: 'indent' };
  return null;
}
