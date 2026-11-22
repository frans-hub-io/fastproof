import type { CaseStep, CaseWrite, FieldValue } from '../../api/tm';

function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
}

function sameTags(a: readonly string[] | null | undefined, b: readonly string[] | null | undefined): boolean {
  return same([...(a ?? [])].sort(), [...(b ?? [])].sort());
}

/** Takes `mine` where the user changed the value (compared with `base`), else `theirs`. */
function pick<T>(base: T, mine: T, theirs: T, eq: (a: T, b: T) => boolean = same): T {
  return eq(mine, base) ? theirs : mine;
}

type StepContent = Pick<CaseStep, 'action' | 'expected' | 'data' | 'attachments'>;

function content(s: CaseStep): StepContent {
  return { action: s.action, expected: s.expected, data: s.data, attachments: s.attachments };
}

function index(steps: readonly CaseStep[], parent: string | null = null, out = new Map<string, { step: CaseStep; parent: string | null; prev: string | null }>()) {
  steps.forEach((s, i) => {
    out.set(s.id, { step: s, parent, prev: i > 0 ? (steps[i - 1]?.id ?? null) : null });
    index(s.steps, s.id, out);
  });
  return out;
}

/**
 * Three-way merge of step trees by step id: the structure (order, nesting, removals) follows `mine`; a step the user
 * did not edit takes the other writer's text; a step the other writer removed and the user left alone goes away (its
 * kept children take its place); steps the other writer added are kept next to their previous sibling.
 */
export function mergeSteps(base: readonly CaseStep[], mine: readonly CaseStep[], theirs: readonly CaseStep[]): CaseStep[] {
  if (same(mine, base)) return structuredClone([...theirs]);
  if (same(theirs, base)) return structuredClone([...mine]);
  const b = index(base);
  const t = index(theirs);
  const m = index(mine);
  const build = (list: readonly CaseStep[]): CaseStep[] =>
    list.flatMap((s) => {
      const children = build(s.steps);
      const was = b.get(s.id);
      const now = t.get(s.id);
      const untouched = !!was && same(content(s), content(was.step));
      if (was && !now && untouched) return children;
      const text = untouched && now ? content(now.step) : content(s);
      return [{ id: s.id, ...structuredClone(text), steps: children }];
    });
  const out = build(mine);
  const placed = index(out);
  const added = [...t.values()].filter((x) => !b.has(x.step.id) && !m.has(x.step.id));
  for (const a of added) {
    if (placed.has(a.step.id)) continue;
    const parent = a.parent ? placed.get(a.parent)?.step : undefined;
    const siblings = parent ? parent.steps : out;
    const copy: CaseStep = { ...structuredClone(content(a.step)), id: a.step.id, steps: [] };
    const at = a.prev ? siblings.findIndex((x) => x.id === a.prev) : -1;
    if (at >= 0) siblings.splice(at + 1, 0, copy);
    else if (a.prev === null) siblings.unshift(copy);
    else siblings.push(copy);
    placed.set(copy.id, { step: copy, parent: parent?.id ?? null, prev: null });
  }
  return out;
}

/**
 * Three-way merge of a case write: for every top-level value (name, status, folder, tags, description,
 * preconditions, links), every custom field key and every step id, the user's value wins only where the user changed
 * it since `base`; everything else comes from `theirs`, the version saved meanwhile.
 */
export function mergeCaseWrite(base: CaseWrite, mine: CaseWrite, theirs: CaseWrite): CaseWrite {
  const bf = base.fields ?? {};
  const mf = mine.fields ?? {};
  const tf = theirs.fields ?? {};
  const fields: Record<string, FieldValue> = {};
  for (const k of new Set([...Object.keys(bf), ...Object.keys(mf), ...Object.keys(tf)])) {
    const v = same(mf[k], bf[k]) ? tf[k] : mf[k];
    if (v !== undefined) fields[k] = v;
  }
  return {
    name: pick(base.name, mine.name, theirs.name),
    status: pick(base.status, mine.status, theirs.status),
    folderId: pick(base.folderId ?? null, mine.folderId ?? null, theirs.folderId ?? null),
    tags: [...pick(base.tags ?? [], mine.tags ?? [], theirs.tags ?? [], sameTags)],
    fields,
    body: {
      description: pick(base.body.description, mine.body.description, theirs.body.description),
      preconditions: pick(base.body.preconditions, mine.body.preconditions, theirs.body.preconditions),
      links: structuredClone(pick(base.body.links, mine.body.links, theirs.body.links)),
      steps: mergeSteps(base.body.steps, mine.body.steps, theirs.body.steps),
    },
  };
}

/** Top-level keys both writers changed to different values (the user's value is kept for them). */
export function bothChanged(base: CaseWrite, mine: CaseWrite, theirs: CaseWrite): string[] {
  const out: string[] = [];
  const check = (key: string, b: unknown, m: unknown, t: unknown, eq: (x: unknown, y: unknown) => boolean = same) => {
    if (!eq(m, b) && !eq(t, b) && !eq(m, t)) out.push(key);
  };
  check('name', base.name, mine.name, theirs.name);
  check('status', base.status, mine.status, theirs.status);
  check('folderId', base.folderId ?? null, mine.folderId ?? null, theirs.folderId ?? null);
  check('tags', base.tags, mine.tags, theirs.tags, (x, y) => sameTags(x as string[], y as string[]));
  check('description', base.body.description, mine.body.description, theirs.body.description);
  check('preconditions', base.body.preconditions, mine.body.preconditions, theirs.body.preconditions);
  check('links', base.body.links, mine.body.links, theirs.body.links);
  const keys = new Set([...Object.keys(base.fields ?? {}), ...Object.keys(mine.fields ?? {}), ...Object.keys(theirs.fields ?? {})]);
  for (const k of keys) check(`fields.${k}`, base.fields?.[k], mine.fields?.[k], theirs.fields?.[k]);
  const bs = index(base.body.steps);
  const ts = index(theirs.body.steps);
  for (const [id, x] of index(mine.body.steps)) {
    const was = bs.get(id);
    const now = ts.get(id);
    if (was && now && !same(content(x.step), content(was.step)) && !same(content(now.step), content(was.step)) && !same(content(x.step), content(now.step))) out.push(`step ${id}`);
  }
  return out;
}
