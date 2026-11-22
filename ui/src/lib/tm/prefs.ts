import type { CaseWrite } from '../../api/tm';

// Browser storage is a convenience only: every read and write may throw (private mode, blocked site data).

function read(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string | null): void {
  try {
    if (value === null) window.localStorage.removeItem(key);
    else window.localStorage.setItem(key, value);
  } catch {
    /* storage unavailable */
  }
}

export interface CaseDraft {
  /** Revision the draft was made on. */
  revision: number;
  savedAt: string;
  body: CaseWrite;
}

const MAX_DRAFT_BYTES = 1_200_000;

function draftKey(project: string, n: number): string {
  return `fastproof.tm.draft.${project}.${String(n)}`;
}

export function loadDraft(project: string, n: number): CaseDraft | null {
  const raw = read(draftKey(project, n));
  if (!raw) return null;
  try {
    const d = JSON.parse(raw) as Partial<CaseDraft>;
    const inner = (d.body as { body?: { steps?: unknown } } | undefined)?.body;
    if (typeof d.revision !== 'number' || !d.body || typeof d.body.name !== 'string' || !Array.isArray(inner?.steps)) return null;
    return { revision: d.revision, savedAt: typeof d.savedAt === 'string' ? d.savedAt : '', body: d.body };
  } catch {
    return null;
  }
}

export function saveDraft(project: string, n: number, d: CaseDraft): void {
  const text = JSON.stringify(d);
  if (text.length > MAX_DRAFT_BYTES) return;
  write(draftKey(project, n), text);
}

export function clearDraft(project: string, n: number): void {
  write(draftKey(project, n), null);
}

const SINGLE_KEYS = 'fastproof.tm.singleKeyShortcuts';

/** Single-character shortcuts of the runner (on unless the user turned them off). */
export function singleKeysEnabled(): boolean {
  return read(SINGLE_KEYS) !== 'off';
}

export function setSingleKeysEnabled(on: boolean): void {
  write(SINGLE_KEYS, on ? null : 'off');
  window.dispatchEvent(new Event('fastproof-prefs'));
}
