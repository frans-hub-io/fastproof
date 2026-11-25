import type { CaseListState } from './filters';

/** FPQL helpers: serializing the filter chips, a small tokenizer for highlighting, and the completion context. */

export type TokenRole = 'keyword' | 'attribute' | 'fieldkey' | 'string' | 'number' | 'function' | 'operator' | 'punct' | 'value' | 'error';

export interface FpqlToken {
  role: TokenRole;
  from: number;
  to: number;
  text: string;
  closed?: boolean;
}

export type FpqlState = 'attr' | 'fieldkey' | 'op' | 'isrest' | 'value' | 'listvalue' | 'listsep' | 'logic' | 'call';

export interface FpqlAnalysis {
  tokens: FpqlToken[];
  state: FpqlState;
  attr: string | null;
  fieldKey: string | null;
  openString: FpqlToken | null;
}

const WORD = /^[A-Za-z0-9_][A-Za-z0-9_.:/-]*/;
const BARE = /^[A-Za-z0-9_][A-Za-z0-9_.:/-]*$/;
const NUMERIC = /^(\d+(\.\d+)?|\d+[dhm]|\d{4}-\d{2}-\d{2})$/;
const OPS = ['!=', '!~', '>=', '<=', '=', '~', '>', '<'];

export function quoteValue(s: string): string {
  return `"${s.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`;
}

/** A bare word when the value is safe as one, otherwise quoted. */
export function valueText(s: string): string {
  return BARE.test(s) && !/^(and|or|not|in|is|empty|true|false)$/i.test(s) ? s : quoteValue(s);
}

export function tokenize(text: string): FpqlToken[] {
  const out: FpqlToken[] = [];
  let i = 0;
  while (i < text.length) {
    const c = text.charAt(i);
    if (/\s/.test(c)) {
      i++;
      continue;
    }
    if (c === '"') {
      let j = i + 1;
      let closed = false;
      while (j < text.length) {
        if (text.charAt(j) === '\\') j += 2;
        else if (text.charAt(j) === '"') {
          closed = true;
          j++;
          break;
        } else j++;
      }
      j = Math.min(j, text.length);
      out.push({ role: 'string', from: i, to: j, text: text.slice(i, j), closed });
      i = j;
      continue;
    }
    const op = OPS.find((o) => text.startsWith(o, i));
    if (op) {
      out.push({ role: 'operator', from: i, to: i + op.length, text: op });
      i += op.length;
      continue;
    }
    if ('()[],'.includes(c)) {
      out.push({ role: 'punct', from: i, to: i + 1, text: c });
      i++;
      continue;
    }
    const w = WORD.exec(text.slice(i));
    if (w) {
      out.push({ role: 'value', from: i, to: i + w[0].length, text: w[0] });
      i += w[0].length;
      continue;
    }
    out.push({ role: c === '-' ? 'operator' : 'error', from: i, to: i + 1, text: c });
    i++;
  }
  return out;
}

interface Frame {
  kind: 'call' | 'group' | 'list';
  ret: FpqlState;
}

/** Classifies the tokens by position in the grammar and returns the state after the last one. */
export function analyze(text: string): FpqlAnalysis {
  const raw = tokenize(text);
  const tokens: FpqlToken[] = [];
  const stack: Frame[] = [];
  let state: FpqlState = 'attr';
  let attr: string | null = null;
  let fieldKey: string | null = null;
  let openString: FpqlToken | null = null;
  const afterValue = (): FpqlState => (state === 'listvalue' ? 'listsep' : state === 'call' ? 'call' : 'logic');
  for (let n = 0; n < raw.length; n++) {
    const t = raw[n];
    if (!t) continue;
    const next = raw[n + 1];
    const lw = t.text.toLowerCase();
    let role: TokenRole = t.role;
    if (t.role === 'string') {
      if (!t.closed) openString = t;
      if (state === 'fieldkey') {
        role = 'fieldkey';
        fieldKey = unquote(t.text);
      } else if (state === 'value' || state === 'listvalue' || state === 'call') {
        state = afterValue();
      }
    } else if (t.role === 'value') {
      if (state === 'attr') {
        if (lw === 'not') role = 'keyword';
        else {
          role = 'attribute';
          attr = t.text;
          fieldKey = null;
          state = 'op';
        }
      } else if (state === 'fieldkey') {
        role = 'fieldkey';
        fieldKey = t.text;
      } else if (state === 'op') {
        if (lw === 'in') {
          role = 'keyword';
          state = 'value';
        } else if (lw === 'not') role = 'keyword';
        else if (lw === 'is') {
          role = 'keyword';
          state = 'isrest';
        }
      } else if (state === 'isrest') {
        if (lw === 'not') role = 'keyword';
        else if (lw === 'empty') {
          role = 'keyword';
          state = 'logic';
        }
      } else if (state === 'value' || state === 'listvalue' || state === 'call') {
        if (next?.text === '(' && next.from === t.to) role = 'function';
        else {
          role = lw === 'true' || lw === 'false' ? 'keyword' : NUMERIC.test(t.text) ? 'number' : 'value';
          state = afterValue();
        }
      } else if (state === 'logic') {
        if (lw === 'and' || lw === 'or') {
          role = 'keyword';
          state = 'attr';
        }
      } else {
        role = 'error';
      }
    } else if (t.role === 'punct') {
      const prev = tokens[tokens.length - 1];
      if (t.text === '(') {
        if (prev?.role === 'function') {
          stack.push({ kind: 'call', ret: state === 'listvalue' ? 'listsep' : 'logic' });
          state = 'call';
        } else if (state === 'attr') stack.push({ kind: 'group', ret: 'logic' });
      } else if (t.text === ')') {
        const f = stack.pop();
        if (f) state = f.ret;
      } else if (t.text === '[') {
        if (state === 'op' && attr?.toLowerCase() === 'field') state = 'fieldkey';
        else if (state === 'value') {
          stack.push({ kind: 'list', ret: 'logic' });
          state = 'listvalue';
        }
      } else if (t.text === ']') {
        if (state === 'fieldkey') state = 'op';
        else if (stack[stack.length - 1]?.kind === 'list') {
          stack.pop();
          state = 'logic';
        }
      } else if (t.text === ',' && state === 'listsep') state = 'listvalue';
    } else if (t.role === 'operator') {
      if (t.text === '-') {
        if (state === 'logic') state = 'value';
      } else if (state === 'op') state = 'value';
    }
    tokens.push({ ...t, role });
  }
  return { tokens, state, attr, fieldKey, openString: openString && openString === raw[raw.length - 1] ? openString : null };
}

function unquote(s: string): string {
  const body = s.endsWith('"') && s.length > 1 ? s.slice(1, -1) : s.slice(1);
  return body.replace(/\\(.)/g, '$1');
}

/** Shape of `QuerySchema` the completion needs (kept structural so tests need no generated types). */
export interface CompletionSchema {
  attributes: { name: string; type: string; operators: string[]; values?: string[] | null; description: string }[];
  functions: { name: string; signature: string; description: string }[];
  fields: { key: string; name: string; type: string; values?: string[] | null }[];
  keywords: string[];
}

export interface Suggestion {
  label: string;
  insert: string;
  detail?: string;
  kind: 'attribute' | 'operator' | 'value' | 'function' | 'keyword' | 'field';
  cursorBack?: number;
}

export interface CompletionResult {
  from: number;
  options: Suggestion[];
}

/** What to offer at the end of `before` (the text left of the cursor). */
export function completionsAt(before: string, schema: CompletionSchema): CompletionResult | null {
  const open = analyze(before).openString;
  let from = before.length;
  let quoted = false;
  if (open) {
    from = open.from + 1;
    quoted = true;
  } else {
    const m = /[A-Za-z0-9_][A-Za-z0-9_.:/-]*$/.exec(before);
    if (m) from = before.length - m[0].length;
  }
  const a = analyze(before.slice(0, quoted ? open?.from : from));
  const options: Suggestion[] = [];
  const attrDef = a.attr ? schema.attributes.find((x) => x.name.toLowerCase() === a.attr?.toLowerCase()) : undefined;
  const wrap = (v: string): string => (quoted ? `${v.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"` : valueText(v));
  switch (a.state) {
    case 'attr':
      for (const at of schema.attributes) {
        const isField = at.name === 'field';
        options.push({ label: at.name, insert: isField ? 'field[' : at.name, detail: at.description, kind: 'attribute' });
      }
      options.push({ label: 'not', insert: 'not ', kind: 'keyword' });
      break;
    case 'fieldkey':
      for (const f of schema.fields) {
        options.push({ label: f.key, insert: `${f.key}]`, detail: f.name, kind: 'field' });
        if (f.name !== f.key) options.push({ label: quoteValue(f.name), insert: `${quoteValue(f.name)}]`, detail: f.key, kind: 'field' });
      }
      break;
    case 'op':
      for (const op of attrDef?.operators ?? ['=', '!=', '~', '!~', 'in', 'is empty']) {
        options.push({ label: op, insert: `${op} `, kind: 'operator' });
        if (op === 'in') options.push({ label: 'not in', insert: 'not in ', kind: 'operator' });
      }
      break;
    case 'isrest':
      options.push({ label: 'empty', insert: 'empty', kind: 'keyword' }, { label: 'not empty', insert: 'not empty', kind: 'keyword' });
      break;
    case 'value':
    case 'listvalue':
    case 'call': {
      const fieldValues = a.fieldKey ? schema.fields.find((f) => f.key === a.fieldKey || f.name === a.fieldKey)?.values : undefined;
      for (const v of fieldValues ?? attrDef?.values ?? []) options.push({ label: v, insert: wrap(v), kind: 'value' });
      if (!quoted && a.state !== 'call') {
        if (attrDef?.type === 'bool') options.push({ label: 'true', insert: 'true', kind: 'keyword' }, { label: 'false', insert: 'false', kind: 'keyword' });
        const takesFunctions = !attrDef || ['date', 'user', 'folder', 'field', 'ref'].includes(attrDef.type);
        for (const f of takesFunctions ? schema.functions : []) {
          const args = /\(\s*\)/.test(f.signature) ? '' : '()';
          options.push({ label: f.name, insert: args ? `${f.name}("")` : `${f.name}()`, detail: f.signature, kind: 'function', cursorBack: args ? 2 : 0 });
        }
      }
      break;
    }
    case 'logic':
      options.push({ label: 'and', insert: 'and ', kind: 'keyword' }, { label: 'or', insert: 'or ', kind: 'keyword' });
      break;
    default:
      break;
  }
  if (!options.length) return null;
  return { from, options };
}

/** Result of turning the filter chips into a query. */
export interface ChipsQuery {
  query: string;
  note: string | null;
}

export const SHORT_TEXT_NOTE = 'The search text is shorter than 3 characters, so it was left out of the query.';

/** Serializes the case-list chips to FPQL, conditions joined by `and`. */
export function chipsToQuery(s: CaseListState): ChipsQuery {
  const parts: string[] = [];
  let note: string | null = null;
  if (s.statuses.length === 1) parts.push(`status = ${valueText(s.statuses[0] ?? '')}`);
  else if (s.statuses.length > 1) parts.push(`status in [${s.statuses.map(valueText).join(', ')}]`);
  if (s.automation) parts.push(`automated = ${s.automation === 'automated' ? 'true' : 'false'}`);
  if (s.aiDraft) parts.push('aiUnreviewed = true');
  if (s.staleDays) parts.push(`staleDays > ${String(s.staleDays)}`);
  for (const t of s.tags) parts.push(`tag = ${quoteValue(t)}`);
  const byKey = new Map<string, string[]>();
  for (const f of s.fields) {
    const i = f.indexOf(':');
    const k = f.slice(0, i);
    byKey.set(k, [...(byKey.get(k) ?? []), f.slice(i + 1)]);
  }
  for (const [k, vs] of byKey) parts.push(`field[${quoteValue(k)}] in [${vs.map(quoteValue).join(', ')}]`);
  const q = s.q.trim();
  if (q) {
    const num = /^(?:tc-?)?(\d{1,9})$/i.exec(q);
    if (num) parts.push(`number = ${String(Number(num[1]))}`);
    else if (Array.from(q).length >= 3) parts.push(`(text ~ ${quoteValue(q)} or name ~ ${quoteValue(q)})`);
    else note = SHORT_TEXT_NOTE;
  }
  return { query: parts.join(' and '), note };
}

export function countLabel(count: number, capped: boolean): string {
  if (capped) return '10 000+';
  return String(count).replace(/\B(?=(\d{3})+(?!\d))/g, ' ');
}

export function errorText(e: { message: string; expected: string[] }): string {
  const expected = e.expected.slice(0, 6);
  return expected.length ? `${e.message}. Expected: ${expected.join(', ')}.` : e.message;
}
