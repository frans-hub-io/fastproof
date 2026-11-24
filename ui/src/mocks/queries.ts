import { http, HttpResponse, type HttpHandler } from 'msw';
import type { QueryAttribute, QueryError, QuerySchema, SavedQuery, SavedQueryPatch, SavedQueryWrite } from '../api/queries';
import { tokenize, type FpqlToken } from '../lib/tm/fpql';
import { tmState } from './tm';
import { ME, TM_PROJECT, type MockCase } from './tmFixtures';

const P = '/api/v1/projects/:key';

const attr = (name: string, type: QueryAttribute['type'], operators: string[], description: string, values?: string[]): QueryAttribute => ({
  name,
  type,
  operators,
  description,
  ...(values ? { values } : {}),
});

const CMP = ['=', '!=', '>', '>=', '<', '<='];
const TEXT = ['=', '!=', '~', '!~', 'in', 'is empty', 'is not empty'];
const EQ = ['=', '!='];

const ATTRIBUTES: QueryAttribute[] = [
  attr('number', 'number', [...CMP, 'in'], 'Case number, 12 or TC-12'),
  attr('name', 'string', TEXT, 'Case name'),
  attr('text', 'text', ['~', '!~'], 'Full text of the case'),
  attr('status', 'enum', [...EQ, 'in'], 'Lifecycle status', ['draft', 'active', 'outdated']),
  attr('state', 'enum', [...EQ, 'in'], 'Review state', ['new', 'in_review', 'approved']),
  attr('automated', 'bool', EQ, 'Has a linked automated test'),
  attr('folder', 'folder', [...EQ, 'in'], 'Folder, by path or subtree("path")'),
  attr('tag', 'string', [...EQ, 'in', 'is empty', 'is not empty'], 'Tag'),
  attr('field', 'field', [...TEXT, ...CMP.slice(2)], 'Custom field, field[key] or field["Name"]'),
  attr('createdBy', 'user', [...EQ, 'in'], 'Author, me() for yourself'),
  attr('updatedBy', 'user', [...EQ, 'in'], 'Last editor'),
  attr('createdAt', 'date', CMP, 'Creation date'),
  attr('updatedAt', 'date', CMP, 'Last change'),
  attr('origin', 'enum', [...EQ, 'in'], 'Where the case came from', ['ui', 'code', 'import', 'ai']),
  attr('aiUnreviewed', 'bool', EQ, 'AI draft nobody has reviewed'),
  attr('review', 'enum', [...EQ, 'in'], 'Review status', ['pending', 'accepted', 'rejected']),
  attr('lastStatus', 'enum', [...EQ, 'in'], 'Status of the last result', ['untested', 'in_progress', 'passed', 'failed', 'blocked', 'skipped']),
  attr('lastRunAt', 'date', CMP, 'When the case last ran'),
  attr('lastSource', 'string', [...EQ, 'in'], 'Where the last result came from'),
  attr('requirement', 'ref', [...EQ, 'in'], 'Linked requirement, REQ-12'),
  attr('sharedStep', 'ref', [...EQ, 'in'], 'Uses a shared step, SS-3'),
  attr('plan', 'ref', [...EQ, 'in'], 'In a test plan, by id or name'),
  attr('hasParameters', 'bool', EQ, 'Has a parameter table'),
  attr('staleDays', 'number', CMP, 'Days since the last run'),
  attr('archived', 'bool', EQ, 'In the trash'),
  attr('filter', 'ref', EQ, 'Matches a saved query, by name'),
];

function schemaOf(): QuerySchema {
  return {
    entity: 'cases',
    attributes: ATTRIBUTES,
    functions: [
      { name: 'now', signature: 'now() - 7d', description: 'The current time; subtract d, h or m' },
      { name: 'me', signature: 'me()', description: 'The signed-in user' },
      { name: 'subtree', signature: 'subtree("path" | id)', description: 'A folder and everything below it' },
    ],
    fields: tmState()
      .fields.filter((f) => !f.archived)
      .map((f) => ({ key: f.key, name: f.name, type: f.type, values: f.options.filter((o) => !o.archived).map((o) => o.value) })),
    keywords: ['and', 'or', 'not', 'in', 'is', 'empty', 'true', 'false'],
  };
}

interface Val {
  text: string;
  fn?: boolean;
}

type Node = { t: 'and'; a: Node; b: Node } | { t: 'or'; a: Node; b: Node } | { t: 'not'; a: Node } | { t: 'cond'; attr: string; key: string | null; op: string; value: Val[] };

class Fail extends Error {
  constructor(readonly err: QueryError) {
    super(err.message);
  }
}

function unquote(s: string): string {
  return s.slice(1, s.endsWith('"') && s.length > 1 ? -1 : undefined).replace(/\\(.)/g, '$1');
}

function parse(text: string): Node | null {
  const toks = tokenize(text);
  let i = 0;
  const fail = (t: FpqlToken | undefined, expected: string[], message: string): never => {
    throw new Fail({ position: t ? t.from : text.length, length: t ? t.to - t.from : 0, expected, near: t?.text ?? '', message });
  };
  for (const t of toks) {
    if (t.role === 'string' && !t.closed) fail(t, ['"'], 'the string is not closed');
    if (t.role === 'error') fail(t, [], `unexpected character ${t.text}`);
  }
  const peek = () => toks[i];
  const word = (w: string) => peek()?.role === 'value' && peek()?.text.toLowerCase() === w;
  const punct = (c: string) => peek()?.role === 'punct' && peek()?.text === c;
  const expectPunct = (c: string) => {
    if (!punct(c)) fail(peek(), [c], c === ')' ? 'the parenthesis is not closed' : `expected ${c}`);
    i++;
  };
  const value = (): Val => {
    const t = peek();
    if (!t || (t.role !== 'string' && t.role !== 'value')) return fail(t, ['a value'], 'expected a value');
    i++;
    if (t.role === 'value' && punct('(') && toks[i]?.from === t.to) {
      i++;
      let depth = 1;
      while (i < toks.length && depth > 0) {
        if (punct('(')) depth++;
        if (punct(')')) depth--;
        i++;
      }
      if (depth > 0) fail(undefined, [')'], 'the parenthesis is not closed');
      if (peek()?.text === '-') {
        i++;
        if (peek()?.role !== 'value') fail(peek(), ['7d'], 'expected a duration such as 7d');
        i++;
      }
      return { text: `${t.text}()`, fn: true };
    }
    return { text: t.role === 'string' ? unquote(t.text) : t.text };
  };
  const cond = (): Node => {
    const at = peek();
    if (!at || at.role !== 'value') return fail(at, ATTRIBUTES.map((a) => a.name).slice(0, 8), 'expected an attribute');
    const def = ATTRIBUTES.find((a) => a.name.toLowerCase() === at.text.toLowerCase());
    if (!def) return fail(at, ATTRIBUTES.map((a) => a.name).slice(0, 8), `unknown attribute ${at.text}`);
    i++;
    let key: string | null = null;
    if (def.name === 'field') {
      expectPunct('[');
      const k = peek();
      if (!k || (k.role !== 'value' && k.role !== 'string')) return fail(k, ['a field key'], 'expected a field key');
      key = k.role === 'string' ? unquote(k.text) : k.text;
      i++;
      expectPunct(']');
    }
    const ot = peek();
    let op: string;
    if (ot?.role === 'operator') {
      op = ot.text;
      i++;
    } else if (word('in')) {
      op = 'in';
      i++;
    } else if (word('not')) {
      i++;
      if (!word('in')) return fail(peek(), ['in'], 'expected in');
      i++;
      op = 'not in';
    } else if (word('is')) {
      i++;
      let not = false;
      if (word('not')) {
        not = true;
        i++;
      }
      if (!word('empty')) return fail(peek(), ['empty'], 'expected empty');
      i++;
      op = not ? 'is not empty' : 'is empty';
    } else return fail(ot, def.operators, `expected an operator after ${def.name}`);
    if (!def.operators.includes(op === 'not in' ? 'in' : op)) return fail(ot, def.operators, `${op} is not supported for ${def.name}`);
    const vals: Val[] = [];
    if (op === 'is empty' || op === 'is not empty') return { t: 'cond', attr: def.name, key, op, value: vals };
    if (op === 'in' || op === 'not in') {
      if (punct('[')) {
        i++;
        vals.push(value());
        while (punct(',')) {
          i++;
          vals.push(value());
        }
        expectPunct(']');
      } else vals.push(value());
    } else {
      const vt = peek();
      vals.push(value());
      if ((op === '~' || op === '!~') && !vals[0]?.fn && Array.from(vals[0]?.text ?? '').length < 3) fail(vt, [], '~ needs at least 3 characters');
    }
    return { t: 'cond', attr: def.name, key, op, value: vals };
  };
  const unary = (): Node => {
    if (word('not')) {
      i++;
      return { t: 'not', a: unary() };
    }
    if (punct('(')) {
      i++;
      const n = or();
      expectPunct(')');
      return n;
    }
    return cond();
  };
  const and = (): Node => {
    let n = unary();
    while (word('and')) {
      i++;
      n = { t: 'and', a: n, b: unary() };
    }
    return n;
  };
  const or = (): Node => {
    let n = and();
    while (word('or')) {
      i++;
      n = { t: 'or', a: n, b: and() };
    }
    return n;
  };
  if (!toks.length) return null;
  const n = or();
  if (i < toks.length) {
    const t = peek();
    fail(t, t?.text === ')' ? [] : ['and', 'or'], t?.text === ')' ? 'the parenthesis was not opened' : 'expected and or or');
  }
  return n;
}

/** The first problem of a query, or null when it parses. */
export function queryProblem(text: string): QueryError | null {
  try {
    parse(text);
    return null;
  } catch (e) {
    if (e instanceof Fail) return e.err;
    throw e;
  }
}

function attrValue(c: MockCase, a: string, key: string | null): string | number | boolean | string[] | null | undefined {
  switch (a) {
    case 'number':
      return c.number;
    case 'name':
    case 'text':
      return c.name;
    case 'status':
      return c.status;
    case 'origin':
      return c.origin;
    case 'automated':
      return c.identities.some((x) => x.kind === 'code');
    case 'aiUnreviewed':
      return c.aiUnreviewed;
    case 'archived':
      return c.archivedAt !== null;
    case 'tag':
      return c.tags;
    case 'field': {
      const def = tmState().fields.find((f) => f.key === key || f.name === key);
      const v = def ? c.fields[def.key] : undefined;
      if (v === undefined || v === null) return null;
      return Array.isArray(v) ? v.map(String) : typeof v === 'object' ? undefined : v;
    }
    default:
      return undefined;
  }
}

const same = (v: unknown, w: string): boolean => {
  const norm = (s: string) => s.toLowerCase().replace(/^tc-/, '');
  return norm(String(v)) === norm(w);
};

function evalNode(n: Node, c: MockCase): boolean {
  if (n.t === 'and') return evalNode(n.a, c) && evalNode(n.b, c);
  if (n.t === 'or') return evalNode(n.a, c) || evalNode(n.b, c);
  if (n.t === 'not') return !evalNode(n.a, c);
  const v = attrValue(c, n.attr, n.key);
  if (v === undefined) return true;
  const list = Array.isArray(v) ? v : v === null ? [] : [v];
  const texts = n.value.map((x) => x.text);
  switch (n.op) {
    case '=':
      return list.some((x) => same(x, texts[0] ?? ''));
    case '!=':
      return !list.some((x) => same(x, texts[0] ?? ''));
    case 'in':
      return list.some((x) => texts.some((t) => same(x, t)));
    case 'not in':
      return !list.some((x) => texts.some((t) => same(x, t)));
    case '~':
      return list.some((x) =>
        String(x)
          .toLowerCase()
          .includes((texts[0] ?? '').toLowerCase()),
      );
    case '!~':
      return !list.some((x) =>
        String(x)
          .toLowerCase()
          .includes((texts[0] ?? '').toLowerCase()),
      );
    case 'is empty':
      return list.length === 0;
    case 'is not empty':
      return list.length > 0;
    case '>':
      return Number(v) > Number(texts[0]);
    case '>=':
      return Number(v) >= Number(texts[0]);
    case '<':
      return Number(v) < Number(texts[0]);
    case '<=':
      return Number(v) <= Number(texts[0]);
    default:
      return true;
  }
}

/** Evaluates the attributes the mock knows (number, name, text, status, origin, automated, aiUnreviewed, archived, tag, field); others match. */
export function matchesQuery(text: string, c: MockCase): boolean {
  const ast = parse(text);
  return !ast || evalNode(ast, c);
}

let saved: SavedQuery[] = seedSaved();

function seedSaved(): SavedQuery[] {
  const t = '2026-11-01T09:00:00Z';
  return [
    { id: 'sq_1', entity: 'cases', name: 'Active and automated', query: 'status = active and automated = true', owner: ME, shared: false, position: 0, mine: true, createdAt: t, updatedAt: t },
    { id: 'sq_2', entity: 'cases', name: 'Drafts to finish', query: 'status = draft', owner: 'maria@example.com', shared: true, position: 0, mine: false, createdAt: t, updatedAt: t },
  ];
}

export function resetSavedQueries(): void {
  saved = seedSaved();
}

function isWriter(): boolean {
  try {
    return window.localStorage.getItem('fastproof-mock-role') !== 'viewer';
  } catch {
    return true;
  }
}

const err = (status: number, code: string, message: string) => HttpResponse.json({ code, message }, { status });
const forbid = () => err(403, 'forbidden', 'requires permission cases:write');

export const queriesHandlers: HttpHandler[] = [
  http.get(`${P}/query/schema`, ({ params }) => (params.key === TM_PROJECT ? HttpResponse.json(schemaOf()) : err(404, 'not_found', 'project not found'))),

  http.post(`${P}/query/validate`, async ({ request }) => {
    const b = (await request.json()) as { query?: string };
    const text = (b.query ?? '').trim();
    if (new TextEncoder().encode(text).length > 4096)
      return HttpResponse.json({ ok: false, error: { position: 0, length: 1, expected: [], near: '', message: 'the query is longer than 4096 bytes' } });
    const error = queryProblem(text);
    if (error) return HttpResponse.json({ ok: false, error });
    const count = [...tmState().cases.values()].filter((c) => c.archivedAt === null && matchesQuery(text, c)).length;
    return HttpResponse.json({ ok: true, normalized: text, count, countCapped: false });
  }),

  http.get(`${P}/saved-queries`, () => HttpResponse.json({ items: saved.filter((s) => s.mine || s.shared) })),

  http.post(`${P}/saved-queries`, async ({ request }) => {
    const b = (await request.json()) as SavedQueryWrite;
    const name = b.name.trim();
    if (!name || name.length > 100) return err(400, 'invalid_body', 'name must be 1-100 characters');
    if (b.shared && !isWriter()) return forbid();
    if (saved.length >= 50) return err(409, 'limit_reached', 'at most 50 saved queries');
    if (saved.some((s) => s.name.toLowerCase() === name.toLowerCase() && (s.shared || s.mine))) return err(409, 'saved_query_name_taken', 'a saved query with this name exists');
    const now = new Date().toISOString();
    const q: SavedQuery = {
      id: `sq_${String(Date.now())}`,
      entity: 'cases',
      name,
      query: b.query,
      owner: ME,
      shared: !!b.shared,
      position: b.position ?? saved.length,
      mine: true,
      createdAt: now,
      updatedAt: now,
    };
    saved.push(q);
    return HttpResponse.json(q, { status: 201 });
  }),

  http.patch(`${P}/saved-queries/:id`, async ({ params, request }) => {
    const q = saved.find((s) => s.id === params.id && (s.mine || s.shared));
    if (!q) return err(404, 'not_found', 'saved query not found');
    const b = (await request.json()) as SavedQueryPatch;
    if ((q.shared || b.shared) && !isWriter()) return forbid();
    if (b.name !== undefined && b.name !== null) {
      const name = b.name.trim();
      if (!name || name.length > 100) return err(400, 'invalid_body', 'name must be 1-100 characters');
      if (saved.some((s) => s.id !== q.id && s.name.toLowerCase() === name.toLowerCase() && (s.shared || s.mine))) return err(409, 'saved_query_name_taken', 'a saved query with this name exists');
      q.name = name;
    }
    if (b.query != null) q.query = b.query;
    if (b.shared != null) q.shared = b.shared;
    if (b.position != null) q.position = b.position;
    q.updatedAt = new Date().toISOString();
    return HttpResponse.json(q);
  }),

  http.delete(`${P}/saved-queries/:id`, ({ params }) => {
    const q = saved.find((s) => s.id === params.id && (s.mine || s.shared));
    if (!q) return err(404, 'not_found', 'saved query not found');
    if (q.shared && !isWriter()) return forbid();
    saved = saved.filter((s) => s !== q);
    return new HttpResponse(null, { status: 204 });
  }),
];
