import { normalizeKind } from '../api/client';
import type { Attachment } from '../api/types';

export interface PlaywrightDiff {
  kind: 'playwrightDiff';
  name: string;
  expected?: Attachment;
  actual?: Attachment;
  diff?: Attachment;
}

export type AttachmentEntry = { kind: 'single'; attachment: Attachment } | PlaywrightDiff;

const SUFFIX = /^(.*)-(expected|actual|diff)(\.png)?$/i;

const ACTIVE_TYPES = ['text/html', 'application/xhtml+xml', 'image/svg+xml', 'application/xml', 'text/xml'];

export function isActiveContent(a: Pick<Attachment, 'contentType' | 'name'>): boolean {
  const ct = a.contentType.split(';')[0]?.trim().toLowerCase() ?? '';
  if (ACTIVE_TYPES.includes(ct)) return true;
  return /\.(html?|svg|xhtml|xml)$/i.test(a.name);
}

export function attachmentKind(a: Attachment): Attachment['kind'] {
  return normalizeKind(a.kind);
}

export function groupAttachments(list: readonly Attachment[]): AttachmentEntry[] {
  const groups = new Map<string, PlaywrightDiff>();
  const out: AttachmentEntry[] = [];
  for (const a of list) {
    const m = SUFFIX.exec(a.name);
    const isPng = a.contentType.toLowerCase().startsWith('image/png');
    if (m && isPng && attachmentKind(a) === 'image') {
      const base = m[1] ?? a.name;
      const role = (m[2] ?? '').toLowerCase() as 'expected' | 'actual' | 'diff';
      let g = groups.get(base);
      if (!g) {
        g = { kind: 'playwrightDiff', name: base };
        groups.set(base, g);
        out.push(g);
      }
      g[role] = a;
    } else {
      out.push({ kind: 'single', attachment: a });
    }
  }
  return out.flatMap((e): AttachmentEntry[] => {
    if (e.kind !== 'playwrightDiff') return [e];
    const parts = [e.expected, e.actual, e.diff].filter((x): x is Attachment => !!x);
    if (e.expected && e.actual) return [e];
    return parts.map((attachment) => ({ kind: 'single', attachment }));
  });
}
