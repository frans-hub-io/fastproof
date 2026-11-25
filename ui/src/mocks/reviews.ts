// Mock of the case workflow and review API (in-memory), following the server's rules closely enough for the e2e.
import { http, HttpResponse, type HttpHandler } from 'msw';
import type { TmJob } from '../api/tm';
import type { CaseReviewSettings, CaseWorkflowSettings, CaseWorkflowTransition, ReviewBulk, ReviewDecision, ReviewKind, ReviewState, ReviewThread, ReviewVote } from '../api/reviews';
import type { ErrorBody } from '../api/types';
import { settingsOf } from './stageC';
import { mockCaseView, reviewHooks, tmState } from './tm';
import { ME, TM_PROJECT, type MockCase } from './tmFixtures';

const API = '/api/v1/projects/:key';
const MOCK_ROLE_KEY = 'fastproof-mock-role';

export const SUGGESTED_WORKFLOW: CaseWorkflowSettings = {
  statuses: [
    { key: 'draft', name: 'Draft', category: 'draft', color: '#868e96', blocksRuns: true },
    { key: 'in_review', name: 'In review', category: 'draft', color: '#f59f00', blocksRuns: true },
    { key: 'approved', name: 'Approved', category: 'active', color: '#2f9e44', blocksRuns: false },
    { key: 'retired', name: 'Retired', category: 'outdated', color: '#e8590c', blocksRuns: true },
  ],
  manual: {
    initial: 'draft',
    transitions: [
      { from: ['draft'], to: 'in_review' },
      { from: ['in_review'], to: 'draft' },
      { from: ['in_review'], to: 'approved', permission: 'cases:review', requires: ['approved', 'notAuthor'] },
      { from: ['approved'], to: 'retired' },
      { from: ['retired'], to: 'draft' },
    ],
  },
  automated: {
    initial: 'approved',
    transitions: [
      { from: ['approved'], to: 'retired' },
      { from: ['retired'], to: 'approved' },
    ],
  },
};

const DEFAULT_POLICY: CaseReviewSettings = { mode: 'off', approvals: 1, resetOn: 'content', allowSelfApproval: false, imports: 'review' };

interface SharedStep {
  number: number;
  name: string;
  headVersion: number;
  author: string;
  updatedAt: string;
  votes: ReviewVote[];
}

interface Mem {
  workflow: CaseWorkflowSettings | null;
  states: Map<number, string>;
  votes: Map<number, ReviewVote[]>;
  approved: Map<number, number>;
  seeded: boolean;
  shared: SharedStep[];
  jobs: Map<string, TmJob>;
}

function fresh(): Mem {
  return {
    workflow: null,
    states: new Map(),
    votes: new Map(),
    approved: new Map(),
    seeded: false,
    shared: [
      { number: 1, name: 'Sign in as a customer', headVersion: 2, author: 'maria@example.com', updatedAt: new Date(Date.now() - 3 * 3_600_000).toISOString(), votes: [] },
      { number: 2, name: 'Open the cart', headVersion: 1, author: 'oleh@example.com', updatedAt: new Date(Date.now() - 26 * 3_600_000).toISOString(), votes: [] },
    ],
    jobs: new Map(),
  };
}

let mem = fresh();

export function resetReviews(): void {
  mem = fresh();
}

/** Test access to the review mock state. */
export function reviewsState(): Mem {
  return mem;
}

reviewHooks.reset.push(resetReviews);

function role(): 'admin' | 'viewer' {
  try {
    return typeof window !== 'undefined' && window.localStorage.getItem(MOCK_ROLE_KEY) === 'viewer' ? 'viewer' : 'admin';
  } catch {
    return 'admin';
  }
}

function err(status: number, code: string, message: string, details?: unknown): Response {
  const body: ErrorBody = { code, message, ...(details === undefined ? {} : { details }) };
  return HttpResponse.json(body, { status });
}

const forbid = (perm: string) => err(403, 'forbidden', `requires permission ${perm}`, { required: perm });

function policy(): CaseReviewSettings {
  const s = settingsOf(TM_PROJECT) as unknown as { caseReview?: CaseReviewSettings };
  return { ...DEFAULT_POLICY, ...s.caseReview };
}

function emit(event: string, data: unknown): void {
  (window as unknown as { __fastproofMockTm?: { emit: (e: string, d: unknown) => void } }).__fastproofMockTm?.emit(event, data);
}

function etag(revision: number): string {
  return `"r${String(revision)}"`;
}

function isAutomated(c: MockCase): boolean {
  return c.identities.some((i) => i.kind === 'code');
}

function reviewed(c: MockCase): boolean {
  const p = policy();
  return p.mode === 'all' || (p.mode === 'manual' && !isAutomated(c));
}

function headAuthor(c: MockCase): string | null {
  return c.versions[c.versions.length - 1]?.createdBy ?? c.updatedBy;
}

function votesOf(c: MockCase): ReviewVote[] {
  return mem.votes.get(c.number) ?? [];
}

/** Votes on `version`, the latest per reviewer. */
function effective(votes: ReviewVote[], version: number): ReviewVote[] {
  const by = new Map<string, ReviewVote>();
  for (const v of votes) if (v.version === version) by.set(v.reviewer, v);
  return [...by.values()];
}

function stateOf(votes: ReviewVote[], version: number, required: number): { state: ReviewState; approvals: number } {
  const cur = effective(votes, version);
  const approvals = cur.filter((v) => v.decision === 'approve').length;
  if (cur.some((v) => v.decision === 'request_changes')) return { state: 'changes_requested', approvals };
  return { state: approvals >= required ? 'approved' : 'pending', approvals };
}

function seed(): void {
  if (mem.seeded) return;
  mem.seeded = true;
  const st = tmState();
  const list = [...st.cases.values()].filter((c) => !c.archivedAt && !isAutomated(c));
  let i = 0;
  for (const c of list) {
    if (c.versions.length >= 2 && i < 3) {
      const prev = c.versions[c.versions.length - 2];
      if (prev) {
        const t = new Date(Date.now() - (i + 2) * 86_400_000).toISOString();
        mem.votes.set(c.number, [{ reviewer: 'sam@example.com', decision: 'approve', comment: 'Looks right.', version: prev.version, createdAt: t }]);
        mem.approved.set(c.number, prev.version);
        i++;
      }
    }
  }
  const second = list.filter((c) => !mem.votes.has(c.number) && headAuthor(c) !== ME)[2];
  if (second) mem.votes.set(second.number, [{ reviewer: 'maria@example.com', decision: 'request_changes', comment: 'The expected result of step 2 is missing.', version: second.version, createdAt: new Date(Date.now() - 5 * 3_600_000).toISOString() }]);
}

interface View {
  reviewState: ReviewState | null;
  approvals: number;
  required: number;
  approvedVersion: number | null;
}

function viewOf(c: MockCase): View {
  const p = policy();
  if (!reviewed(c)) return { reviewState: null, approvals: 0, required: p.approvals, approvedVersion: mem.approved.get(c.number) ?? null };
  seed();
  const r = stateOf(votesOf(c), c.version, p.approvals);
  if (r.state === 'approved') mem.approved.set(c.number, c.version);
  return { reviewState: r.state, approvals: r.approvals, required: p.approvals, approvedVersion: mem.approved.get(c.number) ?? null };
}

function workflowStateOf(c: MockCase): string | null {
  const wf = mem.workflow;
  if (!wf) return null;
  const set = mem.states.get(c.number);
  if (set && wf.statuses.some((s) => s.key === set)) return set;
  const flow = isAutomated(c) ? wf.automated : wf.manual;
  const initial = wf.statuses.find((s) => s.key === flow.initial);
  if (initial && initial.category === c.status) return initial.key;
  return wf.statuses.find((s) => s.category === c.status)?.key ?? flow.initial;
}

reviewHooks.caseExtras = (c) => {
  const v = viewOf(c);
  return { state: workflowStateOf(c), reviewState: v.reviewState, approvedVersion: v.approvedVersion };
};

function matches(t: CaseWorkflowTransition, from: string, to: string): boolean {
  return (t.from.includes('*') || t.from.includes(from)) && (t.to === '*' || t.to === to);
}

function evaluate(c: MockCase, to: string): string[] {
  const wf = mem.workflow;
  const from = workflowStateOf(c);
  if (!wf || !from) return ['transition'];
  const flow = isAutomated(c) ? wf.automated : wf.manual;
  const t = flow.transitions.find((x) => matches(x, from, to));
  if (!t) return ['transition'];
  const missing: string[] = [];
  if (t.permission === 'cases:review' && role() !== 'admin') missing.push('permission');
  if (t.requires?.includes('approved')) {
    const v = viewOf(c);
    if (v.reviewState !== 'approved' || v.approvedVersion !== c.version) missing.push('approved');
  }
  if (t.requires?.includes('requiredFields') && (!c.name.trim() || c.body.steps.length === 0)) missing.push('requiredFields');
  if (t.requires?.includes('notAuthor') && headAuthor(c) === ME) missing.push('notAuthor');
  return missing;
}

function transitionsOf(c: MockCase) {
  const wf = mem.workflow;
  const from = workflowStateOf(c);
  if (!wf || !from) return [];
  const flow = isAutomated(c) ? wf.automated : wf.manual;
  return wf.statuses
    .filter((s) => s.key !== from && flow.transitions.some((t) => matches(t, from, s.key)))
    .map((s) => {
      const missing = evaluate(c, s.key);
      return { to: s.key, name: s.name, allowed: missing.length === 0, missing };
    });
}

function whyNot(c: MockCase, v: View): string | null {
  if (v.reviewState === null) return 'review_not_enabled';
  if (role() !== 'admin') return 'permission';
  if (c.origin === 'ai' && headAuthor(c) === ME) return 'ai_self_review';
  if (headAuthor(c) === ME && !policy().allowSelfApproval) return 'self_approval';
  if (v.reviewState !== 'pending' || effective(votesOf(c), c.version).some((x) => x.reviewer === ME)) return 'not_pending';
  return null;
}

function caseThread(c: MockCase): ReviewThread {
  const v = viewOf(c);
  const why = whyNot(c, v);
  return {
    kind: 'case',
    number: c.number,
    name: c.name,
    headVersion: c.version,
    approvedVersion: v.approvedVersion,
    reviewState: v.reviewState,
    author: headAuthor(c),
    approvals: v.approvals,
    required: v.required,
    votes: structuredClone(effective(votesOf(c), c.version).concat(votesOf(c).filter((x) => x.version !== c.version)).sort((a, b) => b.createdAt.localeCompare(a.createdAt))),
    canVote: why === null,
    ...(why ? { whyNot: why } : {}),
    transitions: transitionsOf(c),
  };
}

function sharedThread(s: SharedStep): ReviewThread {
  const p = policy();
  const on = p.mode === 'all';
  const r = stateOf(s.votes, s.headVersion, p.approvals);
  const why = !on ? 'review_not_enabled' : role() !== 'admin' ? 'permission' : s.author === ME && !p.allowSelfApproval ? 'self_approval' : r.state !== 'pending' || effective(s.votes, s.headVersion).some((x) => x.reviewer === ME) ? 'not_pending' : null;
  return {
    kind: 'sharedStep',
    number: s.number,
    name: s.name,
    headVersion: s.headVersion,
    approvedVersion: null,
    reviewState: on ? r.state : null,
    author: s.author,
    approvals: r.approvals,
    required: p.approvals,
    votes: structuredClone([...s.votes].sort((a, b) => b.createdAt.localeCompare(a.createdAt))),
    canVote: why === null,
    ...(why ? { whyNot: why } : {}),
    transitions: [],
  };
}

function changedSince(c: MockCase, approved: number | null): string[] {
  const keys = new Set<string>();
  for (const v of c.versions) if (v.version > (approved ?? 0)) for (const k of v.changed) keys.add(k);
  return [...keys];
}

interface QueueRow {
  kind: ReviewKind;
  number: number;
  item: {
    kind: ReviewKind;
    number: number;
    name: string;
    headVersion: number;
    approvedVersion: number | null;
    reviewState: ReviewState;
    author: string | null;
    changed: string[];
    approvals: number;
    required: number;
    folderId: string | null;
    updatedAt: string;
  };
}

function queue(url: URL): QueueRow[] {
  const p = policy();
  if (p.mode === 'off') return [];
  const sp = url.searchParams;
  const kind = sp.get('kind');
  const state = sp.get('state');
  const reviewer = sp.get('reviewer') === 'me';
  const author = sp.get('author') === 'me';
  const folder = sp.get('folder');
  const rows: QueueRow[] = [];
  if (kind !== 'sharedStep') {
    for (const c of tmState().cases.values()) {
      if (c.archivedAt || !reviewed(c)) continue;
      const v = viewOf(c);
      if (!v.reviewState) continue;
      const row: QueueRow = {
        kind: 'case',
        number: c.number,
        item: { kind: 'case', number: c.number, name: c.name, headVersion: c.version, approvedVersion: v.approvedVersion, reviewState: v.reviewState, author: headAuthor(c), changed: changedSince(c, v.approvedVersion), approvals: v.approvals, required: v.required, folderId: c.folderId, updatedAt: c.updatedAt },
      };
      rows.push(row);
    }
  }
  if (kind !== 'case' && p.mode === 'all') {
    for (const s of mem.shared) {
      const t = sharedThread(s);
      if (!t.reviewState) continue;
      rows.push({ kind: 'sharedStep', number: s.number, item: { kind: 'sharedStep', number: s.number, name: s.name, headVersion: s.headVersion, approvedVersion: null, reviewState: t.reviewState, author: s.author, changed: ['steps'], approvals: t.approvals, required: t.required, folderId: null, updatedAt: s.updatedAt } });
    }
  }
  const mock = tmState().cases;
  return rows
    .filter((r) => {
      if (state && r.item.reviewState !== state) return false;
      if (author && r.item.author !== ME) return false;
      if (reviewer) {
        if (r.item.author === ME && !p.allowSelfApproval) return false;
        const votes = r.kind === 'case' ? votesOf(mock.get(r.number) as MockCase) : (mem.shared.find((s) => s.number === r.number)?.votes ?? []);
        if (effective(votes, r.item.headVersion).some((x) => x.reviewer === ME)) return false;
      }
      if (folder && r.item.folderId !== folder) return false;
      return true;
    })
    .sort((a, b) => b.item.updatedAt.localeCompare(a.item.updatedAt) || a.number - b.number);
}

function cursorOf(r: QueueRow): string {
  return `${r.kind}:${String(r.number)}`;
}

function recordVote(votes: ReviewVote[], decision: ReviewDecision, version: number, comment: string): ReviewVote[] {
  const mine: ReviewVote = { reviewer: ME, decision, comment, version, createdAt: new Date().toISOString() };
  return [...votes.filter((x) => !(x.reviewer === ME && x.version === version)), mine];
}

const KEY_RE = /^[a-z][a-z0-9_]{0,31}$/;
const COLOR_RE = /^#[0-9a-fA-F]{6}$/;
const encoder = new TextEncoder();

function validateWorkflow(w: CaseWorkflowSettings): string | null {
  if (w.statuses.length < 1 || w.statuses.length > 20) return 'caseWorkflow.statuses: 1 to 20 statuses';
  const keys = new Set<string>();
  for (const s of w.statuses) {
    if (!KEY_RE.test(s.key)) return `caseWorkflow.statuses.key: "${s.key}" must match ^[a-z][a-z0-9_]{0,31}$`;
    if (keys.has(s.key)) return `caseWorkflow.statuses.key: "${s.key}" is used twice`;
    keys.add(s.key);
    const n = encoder.encode(s.name).length;
    if (n < 1 || n > 100) return `caseWorkflow.statuses.name: 1 to 100 bytes (${s.key})`;
    if (s.color && !COLOR_RE.test(s.color)) return `caseWorkflow.statuses.color: ${s.key} needs #rrggbb`;
  }
  for (const cat of ['draft', 'active', 'outdated']) if (!w.statuses.some((s) => s.category === cat)) return `caseWorkflow.statuses: needs at least one status in the ${cat} category`;
  for (const f of ['manual', 'automated'] as const) {
    const flow = w[f];
    if (!keys.has(flow.initial)) return `caseWorkflow.${f}.initial: "${flow.initial}" is not a status`;
    if (flow.transitions.length > 100) return `caseWorkflow.${f}.transitions: at most 100`;
    for (const t of flow.transitions) {
      if (t.from.length === 0 || t.from.some((x) => x !== '*' && !keys.has(x))) return `caseWorkflow.${f}.transitions.from: unknown status`;
      if (t.to !== '*' && !keys.has(t.to)) return `caseWorkflow.${f}.transitions.to: "${t.to}" is not a status`;
      if ((t.requires?.length ?? 0) > 3) return `caseWorkflow.${f}.transitions.requires: at most 3 guards`;
    }
  }
  return null;
}

export const reviewsHandlers: HttpHandler[] = [
  http.get(`${API}/case-workflow`, ({ params }) => {
    if (params.key !== TM_PROJECT) return err(404, 'not_found', 'project not found');
    return HttpResponse.json({ configured: mem.workflow !== null, workflow: structuredClone(mem.workflow ?? SUGGESTED_WORKFLOW), review: policy() });
  }),

  http.put(`${API}/case-workflow`, async ({ params, request }) => {
    if (params.key !== TM_PROJECT) return err(404, 'not_found', 'project not found');
    if (role() !== 'admin') return forbid('settings:write');
    const body = (await request.json()) as { workflow: CaseWorkflowSettings | null; remap?: Record<string, string> | null };
    const next = body.workflow;
    if (next) {
      const bad = validateWorkflow(next);
      if (bad) return err(400, 'bad_request', bad);
    }
    const cases = [...tmState().cases.values()].filter((c) => !c.archivedAt);
    const moved: { c: MockCase; to: string }[] = [];
    if (next && mem.workflow) {
      const counts = new Map<string, number>();
      for (const c of cases) {
        const cur = workflowStateOf(c);
        if (!cur) continue;
        const kept = next.statuses.find((s) => s.key === cur);
        if (kept && kept.category === c.status) continue;
        counts.set(cur, (counts.get(cur) ?? 0) + 1);
      }
      const remap = body.remap ?? {};
      const open = [...counts.keys()].filter((k) => !remap[k] || !next.statuses.some((s) => s.key === remap[k]));
      if (open.length > 0) return err(409, 'workflow_status_in_use', 'cases still use states the new workflow drops', { statuses: open.map((key) => ({ key, cases: counts.get(key) ?? 0 })) });
      for (const c of cases) {
        const cur = workflowStateOf(c);
        const target = cur ? remap[cur] : undefined;
        if (cur && target && counts.has(cur)) moved.push({ c, to: target });
      }
    }
    let job: TmJob | undefined;
    if (moved.length > 0 && next) {
      for (const m of moved) {
        mem.states.set(m.c.number, m.to);
        const cat = next.statuses.find((s) => s.key === m.to)?.category;
        if (cat) m.c.status = cat;
      }
      const at = new Date().toISOString();
      job = { id: '0198aaaa-0000-7000-8000-00000000f001', kind: 'workflow_remap', status: 'done', progress: { done: moved.length, total: moved.length }, createdAt: at, finishedAt: at, createdBy: ME };
      mem.jobs.set(job.id, job);
    }
    if (!next) mem.states.clear();
    mem.workflow = next ? structuredClone(next) : null;
    return HttpResponse.json({ view: { configured: mem.workflow !== null, workflow: structuredClone(mem.workflow ?? SUGGESTED_WORKFLOW), review: policy() }, ...(job ? { job } : {}) });
  }),

  http.post(`${API}/cases/:n/transition`, async ({ params, request }) => {
    const c = params.key === TM_PROJECT ? tmState().cases.get(Number(params.n)) : undefined;
    if (!c) return err(404, 'not_found', 'case not found');
    if (role() !== 'admin') return forbid('cases:write');
    const m = request.headers.get('If-Match');
    if (!m) return err(428, 'precondition_required', 'send If-Match with the ETag you read');
    if (m !== etag(c.revision)) return err(412, 'precondition_failed', 'the resource changed since you read it', { currentRevision: c.revision, changedBy: c.updatedBy, source: 'ui' });
    const { to } = (await request.json()) as { to: string };
    const from = workflowStateOf(c);
    const missing = evaluate(c, to);
    if (!mem.workflow || missing.length > 0) return err(409, 'invalid_transition', `the case cannot move from ${from ?? 'its state'} to ${to}`, { from, to, missing });
    mem.states.set(c.number, to);
    const cat = mem.workflow.statuses.find((s) => s.key === to)?.category;
    if (cat) c.status = cat;
    c.revision++;
    c.updatedAt = new Date().toISOString();
    const revision = c.revision;
    setTimeout(() => emit('case.updated', { number: c.number, revision, source: 'ui' }), 150);
    return HttpResponse.json(mockCaseView(c, TM_PROJECT), { headers: { ETag: etag(c.revision) } });
  }),

  http.get(`${API}/cases/:n/reviews`, ({ params }) => {
    const c = params.key === TM_PROJECT ? tmState().cases.get(Number(params.n)) : undefined;
    if (!c) return err(404, 'not_found', 'case not found');
    return HttpResponse.json(caseThread(c), { headers: { ETag: etag(c.revision) } });
  }),

  http.post(`${API}/cases/:n/reviews`, async ({ params, request }) => {
    const c = params.key === TM_PROJECT ? tmState().cases.get(Number(params.n)) : undefined;
    if (!c) return err(404, 'not_found', 'case not found');
    if (role() !== 'admin') return forbid('cases:review');
    const body = (await request.json()) as { version: number; decision: ReviewDecision; comment?: string | null };
    const v = viewOf(c);
    if (v.reviewState === null) return err(409, 'review_not_enabled', 'review is not enabled for this case');
    if (body.version !== c.version) return err(409, 'version_moved', 'the case has a newer version', { currentVersion: c.version });
    const m = request.headers.get('If-Match');
    if (!m) return err(428, 'precondition_required', 'send If-Match with the ETag you read');
    if (m !== etag(c.revision)) return err(412, 'precondition_failed', 'the resource changed since you read it', { currentRevision: c.revision, changedBy: c.updatedBy, source: 'ui' });
    const comment = body.comment ?? '';
    if (encoder.encode(comment).length > 4000) return err(400, 'bad_request', 'comment: at most 4000 bytes');
    const why = whyNot(c, v);
    if (why === 'self_approval') return err(403, 'self_approval', 'you cannot approve your own change');
    if (why === 'ai_self_review') return err(403, 'ai_self_review', 'an AI draft cannot be approved by its own account');
    if (why === 'not_pending') return err(409, 'not_pending', 'this version is not waiting for your review');
    mem.votes.set(c.number, recordVote(votesOf(c), body.decision, c.version, comment));
    const after = viewOf(c);
    emit('case.reviewed', { number: c.number, version: c.version, reviewState: after.reviewState, approvedVersion: after.approvedVersion });
    return HttpResponse.json(caseThread(c), { headers: { ETag: etag(c.revision) } });
  }),

  http.get(`${API}/shared-steps/:n/reviews`, ({ params }) => {
    const s = mem.shared.find((x) => x.number === Number(params.n));
    if (!s) return err(404, 'not_found', 'shared step not found');
    return HttpResponse.json(sharedThread(s), { headers: { ETag: '"r1"' } });
  }),

  http.post(`${API}/shared-steps/:n/reviews`, async ({ params, request }) => {
    const s = mem.shared.find((x) => x.number === Number(params.n));
    if (!s) return err(404, 'not_found', 'shared step not found');
    if (role() !== 'admin') return forbid('cases:review');
    const body = (await request.json()) as { version: number; decision: ReviewDecision; comment?: string | null };
    const t = sharedThread(s);
    if (t.reviewState === null) return err(409, 'review_not_enabled', 'review is not enabled');
    if (body.version !== s.headVersion) return err(409, 'version_moved', 'the shared step has a newer version', { currentVersion: s.headVersion });
    if (t.whyNot === 'self_approval') return err(403, 'self_approval', 'you cannot approve your own change');
    if (t.whyNot === 'not_pending') return err(409, 'not_pending', 'this version is not waiting for your review');
    s.votes = recordVote(s.votes, body.decision, s.headVersion, body.comment ?? '');
    return HttpResponse.json(sharedThread(s), { headers: { ETag: '"r1"' } });
  }),

  http.get(`${API}/reviews`, ({ params, request }) => {
    if (params.key !== TM_PROJECT) return err(404, 'not_found', 'project not found');
    const rows = queue(new URL(request.url));
    const url = new URL(request.url);
    const before = url.searchParams.get('before');
    let start = 0;
    if (before) {
      const i = rows.findIndex((r) => cursorOf(r) === before);
      start = i >= 0 ? i + 1 : rows.length;
    }
    const n = Number(url.searchParams.get('limit') ?? 50);
    const limit = Number.isFinite(n) && n > 0 ? Math.min(500, Math.floor(n)) : 50;
    const slice = rows.slice(start, start + limit);
    const last = slice[slice.length - 1];
    return HttpResponse.json({ items: slice.map((r) => r.item), next: start + limit < rows.length && last ? cursorOf(last) : null });
  }),

  http.post(`${API}/reviews/bulk`, async ({ params, request }) => {
    if (params.key !== TM_PROJECT) return err(404, 'not_found', 'project not found');
    if (role() !== 'admin') return forbid('cases:review');
    const body = (await request.json()) as ReviewBulk;
    if (body.items.length < 1 || body.items.length > 500) return err(400, 'bad_request', 'items: 1 to 500');
    if (encoder.encode(body.comment ?? '').length > 4000) return err(400, 'bad_request', 'comment: at most 4000 bytes');
    const p = policy();
    let applied = 0;
    const skipped: { kind: ReviewKind; number: number; code: string }[] = [];
    for (const it of body.items) {
      const skip = (code: string) => skipped.push({ kind: it.kind, number: it.number, code });
      if (it.kind === 'case') {
        const c = tmState().cases.get(it.number);
        if (!c || c.archivedAt) {
          skip('not_found');
          continue;
        }
        const v = viewOf(c);
        if (v.reviewState === null) skip('review_not_enabled');
        else if (it.version !== c.version) skip('version_moved');
        else if (c.origin === 'ai' && headAuthor(c) === ME) skip('ai_self_review');
        else if (headAuthor(c) === ME && !p.allowSelfApproval) skip('self_approval');
        else if (v.reviewState !== 'pending') skip('not_pending');
        else {
          mem.votes.set(c.number, recordVote(votesOf(c), body.decision, c.version, body.comment ?? ''));
          applied++;
        }
      } else {
        const s = mem.shared.find((x) => x.number === it.number);
        if (!s) skip('not_found');
        else if (p.mode !== 'all') skip('review_not_enabled');
        else if (it.version !== s.headVersion) skip('version_moved');
        else if (s.author === ME && !p.allowSelfApproval) skip('self_approval');
        else if (stateOf(s.votes, s.headVersion, p.approvals).state !== 'pending') skip('not_pending');
        else {
          s.votes = recordVote(s.votes, body.decision, s.headVersion, body.comment ?? '');
          applied++;
        }
      }
    }
    return HttpResponse.json({ applied, skipped });
  }),
];
