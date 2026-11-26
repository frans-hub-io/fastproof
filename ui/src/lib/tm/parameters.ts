import type { CaseParameters, CaseStep, ParameterRow } from '../../api/parameters';

/** `^[A-Za-z_][A-Za-z0-9_.-]{0,63}$`, as the server checks column names. */
export const COLUMN_RE = /^[A-Za-z_][A-Za-z0-9_.-]{0,63}$/;
export const MAX_COLUMNS = 20;
export const MAX_ROWS = 1000;
export const MAX_VALUE_BYTES = 1024;

export const EMPTY_PARAMETERS: CaseParameters = { columns: [], rows: [] };

const PLACEHOLDER = /\$\{([A-Za-z_][A-Za-z0-9_.-]{0,63})\}/g;

/** A placeholder in a text: `${name}` and where it is. */
export interface Placeholder {
  name: string;
  start: number;
  end: number;
}

/** The `${name}` placeholders of a text, in order (anything else between `${` and `}` is plain text). */
export function placeholders(text: string): Placeholder[] {
  const out: Placeholder[] = [];
  for (const m of text.matchAll(PLACEHOLDER)) {
    const name = m[1];
    if (name !== undefined) out.push({ name, start: m.index, end: m.index + m[0].length });
  }
  return out;
}

/**
 * Bytes the replacements of one iteration view may add, in all its texts. The server puts the same limit on the
 * values it substitutes in a projected iteration (1 MiB per call chain); past it the remaining placeholders stay as
 * they are, so a text full of `${a}` with a 1 KiB value cannot grow into megabytes in the browser.
 */
export const SUBSTITUTION_BUDGET = 1024 * 1024;

/** The bytes of replacements still allowed, shared by the calls of one view. */
export interface SubstitutionBudget {
  left: number;
}

export function newBudget(left = SUBSTITUTION_BUDGET): SubstitutionBudget {
  return { left };
}

const encoder = new TextEncoder();

/**
 * `text` with every placeholder of a known name replaced by its value; unknown names stay. With a `budget`, a value
 * that does not fit in what is left (in UTF-8 bytes, as on the server) is not put in and its placeholder stays.
 */
export function substitute(text: string, values: Readonly<Record<string, string>>, budget?: SubstitutionBudget): string {
  return text.replace(PLACEHOLDER, (whole, name: string) => {
    if (!Object.prototype.hasOwnProperty.call(values, name)) return whole;
    const v = values[name] ?? '';
    if (!budget) return v;
    const n = encoder.encode(v).length;
    if (n > budget.left) return whole;
    budget.left -= n;
    return v;
  });
}

/**
 * Sanitized HTML (from the server) with placeholders replaced in its text only: attributes and markup stay as they
 * are, so a value can never become a link target or markup. Values are inserted as text, under the same `budget`.
 */
export function substituteHtml(html: string, values: Readonly<Record<string, string>>, budget?: SubstitutionBudget): string {
  if (!html.includes('${') || typeof document === 'undefined') return html;
  const t = document.createElement('template');
  t.innerHTML = html;
  const walker = document.createTreeWalker(t.content, NodeFilter.SHOW_TEXT);
  const nodes: Text[] = [];
  for (let n = walker.nextNode(); n; n = walker.nextNode()) nodes.push(n as Text);
  for (const n of nodes) {
    const v = n.data;
    if (v.includes('${')) n.data = substitute(v, values, budget);
  }
  return t.innerHTML;
}

/** Placeholders of a case's preconditions and steps that name no column, with their field path. */
export function unknownPlaceholders(preconditions: string, steps: readonly CaseStep[], columns: readonly string[]): { path: string; name: string }[] {
  const known = new Set(columns);
  const out: { path: string; name: string }[] = [];
  const check = (path: string, text: string) => {
    for (const p of placeholders(text)) if (!known.has(p.name)) out.push({ path, name: p.name });
  };
  check('body.preconditions', preconditions);
  const walk = (list: readonly CaseStep[], prefix: string) =>
    list.forEach((s, i) => {
      const p = `${prefix}[${String(i)}]`;
      check(`${p}.action`, s.action);
      check(`${p}.expected`, s.expected);
      check(`${p}.data`, s.data);
      walk(s.steps, `${p}.steps`);
    });
  walk(steps, 'body.steps');
  return out;
}

const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

/** A new row id: `r_` and a ULID (48-bit time, 80 random bits). */
export function newRowId(now = Date.now()): string {
  let time = '';
  let t = now;
  for (let i = 0; i < 10; i++) {
    time = CROCKFORD.charAt(t % 32) + time;
    t = Math.floor(t / 32);
  }
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  let rand = '';
  for (let i = 0; i < 16; i++) rand += CROCKFORD.charAt((bytes[i] ?? 0) % 32);
  return `r_${time}${rand}`;
}

function bytes(s: string): number {
  return new TextEncoder().encode(s).length;
}

/** Problems of a parameters table before it is saved (the server checks the same and more). */
export function tableProblems(p: CaseParameters): string[] {
  const out: string[] = [];
  if (p.columns.length > MAX_COLUMNS) out.push(`At most ${String(MAX_COLUMNS)} columns`);
  if (p.rows.length > MAX_ROWS) out.push(`At most ${String(MAX_ROWS)} rows`);
  const seen = new Set<string>();
  for (const c of p.columns) {
    if (!COLUMN_RE.test(c)) out.push(`“${c}” is not a valid column name: a letter or _ and then letters, digits, _, . or -`);
    if (seen.has(c)) out.push(`Column “${c}” appears twice`);
    seen.add(c);
  }
  const sets = new Map<string, number>();
  p.rows.forEach((r, i) => {
    for (const c of p.columns) if (bytes(r.values[c] ?? '') > MAX_VALUE_BYTES) out.push(`Row ${String(i + 1)}: “${c}” is longer than ${String(MAX_VALUE_BYTES)} bytes`);
    if (!r.enabled) return;
    const key = JSON.stringify(p.columns.map((c) => r.values[c] ?? ''));
    const first = sets.get(key);
    if (first !== undefined) out.push(`Rows ${String(first + 1)} and ${String(i + 1)} have the same values`);
    else sets.set(key, i);
  });
  return out;
}

/**
 * Rows from tab-separated text (pasted from a spreadsheet). With `header`, the first line names the columns (new
 * names are added); otherwise the cells fill the given columns in order. Empty lines are skipped.
 */
export function parseTsv(text: string, columns: readonly string[], header: boolean): { columns: string[]; rows: ParameterRow[] } {
  const lines = text.replace(/\r\n?/g, '\n').split('\n').filter((l) => l.trim() !== '');
  const cols = [...columns];
  let order = [...columns];
  if (header && lines.length) {
    order = (lines.shift() ?? '').split('\t').map((h) => h.trim());
    for (const h of order) if (h && !cols.includes(h)) cols.push(h);
  }
  const rows = lines.map((l) => {
    const cells = l.split('\t');
    const values: Record<string, string> = {};
    order.forEach((c, i) => {
      if (c) values[c] = cells[i] ?? '';
    });
    return { id: newRowId(), values, enabled: true };
  });
  return { columns: cols, rows };
}

/** The table without values of columns that no longer exist. */
export function pruneValues(p: CaseParameters): CaseParameters {
  return {
    ...p,
    rows: p.rows.map((r) => ({ ...r, values: Object.fromEntries(p.columns.map((c) => [c, r.values[c] ?? ''] as const)) })),
  };
}

/** `values` of a generator from one textarea per column (one value per line, blank lines and repeats dropped). */
export function generatorValues(text: Readonly<Record<string, string>>): Record<string, string[]> {
  const out: Record<string, string[]> = {};
  for (const [k, v] of Object.entries(text)) {
    const list = [...new Set(v.split(/\r?\n/).map((x) => x.trim()).filter(Boolean))];
    if (list.length) out[k] = list;
  }
  return out;
}

/** `a=1 · b=2` for a row of values, in column order. */
export function valuesLabel(columns: readonly string[], values: Readonly<Record<string, string>>): string {
  return columns.map((c) => `${c}=${values[c] ?? ''}`).join(' · ');
}
