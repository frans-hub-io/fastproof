import type { Launch, ResultDetail, ResultStatus, ResultSummary, ScheduledJob } from '../api/types';
import type {
  Cluster,
  Dashboard,
  DefectGroup,
  DefectInfo,
  DefectType,
  Delivery,
  FlakyTest,
  GateResult,
  LaunchComparison,
  LogLevel,
  LogLineWire,
  MockInteractions,
  ProjectSettings,
  Rerun,
  RerunList,
  RerunRequest,
  Rule,
  RuleMatch,
  SimilarFailure,
  TestPlanCreate,
  Transition,
  TriageRequest,
} from '../api/types-next';
import { LOG_LEVELS } from '../api/types-next';
import { clientIdOf, launchDef, launchResults, launchView, NOW, projectLaunches, resultDetail, resultEnv, testKind } from './fixtures';

const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;
const ESC = '\u001b';

function iso(t: number): string {
  return new Date(t).toISOString();
}

function hash(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h >>> 0;
}

export const BUILT_IN_TYPES: DefectType[] = [
  { id: 'productBug', group: 'productBug', name: 'Product bug', shortName: 'PB', color: '#e03131', builtIn: true },
  { id: 'automationBug', group: 'automationBug', name: 'Automation bug', shortName: 'AB', color: '#f08c00', builtIn: true },
  { id: 'systemIssue', group: 'systemIssue', name: 'System issue', shortName: 'SI', color: '#1c7ed6', builtIn: true },
  { id: 'noDefect', group: 'noDefect', name: 'No defect', shortName: 'ND', color: '#868e96', builtIn: true },
  { id: 'toInvestigate', group: 'toInvestigate', name: 'To investigate', shortName: 'TI', color: '#ae3ec9', builtIn: true },
];

function initialTypes(): DefectType[] {
  return [
    ...BUILT_IN_TYPES,
    { id: 'ab-locator', group: 'automationBug', name: 'Fragile locator', shortName: 'FL', color: '#fab005', builtIn: false },
    { id: 'si-payment-sandbox', group: 'systemIssue', name: 'Payment sandbox outage', shortName: 'PSO', color: '#4dabf7', builtIn: false },
  ];
}

function initialRules(): Rule[] {
  const t = iso(NOW - 12 * DAY);
  return [
    {
      id: 'rule-gateway',
      name: 'Gateway 5xx is a system issue',
      enabled: true,
      order: 1,
      match: { messageRegex: 'Received: 50[0-9]|Bad Gateway', statuses: ['failed', 'broken'] },
      action: { defectTypeId: 'systemIssue', comment: 'Upstream gateway answered 5xx' },
      createdAt: t,
      updatedAt: t,
      hits: 214,
    },
    {
      id: 'rule-hikari',
      name: 'Connection pool exhausted',
      enabled: true,
      order: 2,
      match: { messageRegex: 'HikariPool-1 - Connection is not available', statuses: ['broken'] },
      action: { defectTypeId: 'systemIssue', comment: 'Test database is overloaded', issue: { url: 'https://issues.example.com/browse/OPS-77', key: 'OPS-77' } },
      createdAt: t,
      updatedAt: iso(NOW - 3 * DAY),
      hits: 61,
    },
    {
      id: 'rule-visual',
      name: 'Accept checkout visual drift',
      enabled: false,
      order: 3,
      match: { messageRegex: 'Screenshot comparison failed', environment: 'staging-chromium' },
      action: { defectTypeId: 'ab-locator', mute: { days: 7 } },
      createdAt: iso(NOW - 2 * DAY),
      updatedAt: iso(NOW - 2 * DAY),
      hits: 0,
    },
  ];
}

function initialSettings(): ProjectSettings {
  return {
    flaky: { historyDepth: 10, stabilizationPeriod: 5, includePassedTests: false },
    retention: { rawDays: 90, launchesKeep: 500, keepImportant: true },
    rollup: { afterDays: 30 },
    gates: [
      {
        name: 'main stays green',
        enabled: true,
        branches: ['main', 'release/*'],
        rules: [
          { metric: 'newFailures', op: 'lte', value: 0 },
          { metric: 'passRate', op: 'gte', value: 95 },
        ],
      },
      {
        name: 'no slow builds',
        enabled: true,
        branches: [],
        rules: [{ metric: 'duration', op: 'lte', value: 1200, baseline: 'main' }],
      },
    ],
    notifications: [
      { id: 'n-slack', kind: 'slack', on: ['gateFailed', 'newFailures'], target: '', targetSecretRef: 'slack-webhook', secretRef: null, filter: { branches: ['main'], environments: [] } },
      { id: 'n-hook', kind: 'webhook', on: ['launchFinished'], target: 'https://ci.example.com/hooks/fastproof', secretRef: 'webhook-hmac', filter: null },
      { id: 'n-mail', kind: 'email', on: ['gateFailed'], target: 'qa-leads@example.com', secretRef: null, filter: { branches: ['release/*'], environments: ['production-smoke'] } },
    ],
    issueTrackers: [
      { type: 'jira', id: 'jira', urlTemplate: 'https://issues.example.com/browse/{key}', keyRegex: '^[A-Z][A-Z0-9]+-\\d+$', api: { url: 'https://issues.example.com', tokenSecretRef: 'jira-token', project: 'SHOP', labels: ['fastproof'], attachScreenshots: true } },
      { type: 'github', urlTemplate: 'https://github.com/example/web-shop/issues/{key}', keyRegex: '^#?\\d+$' },
    ],
    vcs: { provider: 'gitlab', url: 'https://gitlab.example.com', repository: 'example/web-shop', tokenSecretRef: 'gitlab-token', comment: true, status: true },
    fastmock: { url: 'https://fastmock.example.com', tokenSecretRef: 'fastmock-token' },
    rerun: { provider: 'github', url: 'https://api.github.com', repository: 'example/web-shop', workflow: 'tests.yml', ref: 'main', tokenSecretRef: 'gitlab-token' },
  };
}

interface TriageState {
  defect?: DefectInfo | null;
  muted?: boolean;
  muteUntil?: string | null;
  muteReason?: string | null;
}

type MockSecret = { name: string; purpose: string | null; origin: string | null; updatedAt: string };

const state = {
  types: initialTypes(),
  rules: initialRules(),
  settings: new Map<string, ProjectSettings>(),
  secrets: new Map<string, MockSecret[]>(),
  triage: new Map<string, TriageState>(),
  important: new Map<string, boolean>(),
  plans: new Map<string, { project: string; plan: TestPlanCreate }>(),
  ruleSeq: 10,
  typeSeq: 10,
};

export function resetStageC(): void {
  state.types = initialTypes();
  state.rules = initialRules();
  state.settings.clear();
  state.secrets.clear();
  state.triage.clear();
  state.important.clear();
  state.plans.clear();
}

export function defectTypes(): DefectType[] {
  return state.types;
}

export function addDefectType(t: Omit<DefectType, 'id' | 'builtIn'>): DefectType {
  const created: DefectType = { ...t, id: `custom-${String(++state.typeSeq)}`, builtIn: false };
  state.types.push(created);
  return created;
}

export function patchDefectType(id: string, patch: Partial<DefectType>): DefectType | null {
  const t = state.types.find((x) => x.id === id);
  if (!t) return null;
  Object.assign(t, { ...patch, id: t.id, builtIn: t.builtIn, group: t.builtIn ? t.group : (patch.group ?? t.group) });
  return t;
}

export function deleteDefectType(id: string): 'ok' | 'missing' | 'builtIn' {
  const t = state.types.find((x) => x.id === id);
  if (!t) return 'missing';
  if (t.builtIn) return 'builtIn';
  state.types = state.types.filter((x) => x.id !== id);
  return 'ok';
}

function typeInfo(id: string | null | undefined): DefectType | undefined {
  return state.types.find((t) => t.id === id);
}

function defectFor(typeId: string, extra: Partial<DefectInfo>): DefectInfo {
  const t = typeInfo(typeId) ?? BUILT_IN_TYPES[4] ?? { id: 'toInvestigate', group: 'toInvestigate' as const, name: 'To investigate' };
  return { typeId: t.id, group: t.group, name: t.name, comment: null, issueUrl: null, issueKey: null, source: 'user', at: new Date().toISOString(), by: 'alex@example.com', ...extra };
}

export function settingsOf(project: string): ProjectSettings {
  let s = state.settings.get(project);
  if (!s) {
    s = initialSettings();
    if (project !== 'web-shop') {
      s.gates = [];
      s.notifications = [];
      s.vcs = null;
      s.fastmock = null;
      s.rerun = null;
    }
    state.settings.set(project, s);
  }
  return s;
}

/** Like the server: each top-level area in the body replaces the stored one, absent areas are kept and
 * `null` resets an area to its default. */
export function putSettings(project: string, body: Partial<Record<keyof ProjectSettings, unknown>>): ProjectSettings {
  const cur = settingsOf(project) as unknown as Record<string, unknown>;
  const defaults = initialSettings() as unknown as Record<string, unknown>;
  const next: Record<string, unknown> = { ...cur };
  for (const [k, v] of Object.entries(body)) {
    if (v !== null) next[k] = structuredClone(v);
    else if (k === 'flaky' || k === 'retention' || k === 'rollup') next[k] = structuredClone(defaults[k]);
    else if (k === 'gates' || k === 'notifications' || k === 'issueTrackers') next[k] = [];
    else next[k] = null;
  }
  state.settings.set(project, next as unknown as ProjectSettings);
  return settingsOf(project);
}

export function secretsOf(project: string): MockSecret[] {
  let list = state.secrets.get(project);
  if (!list) {
    list =
      project === 'web-shop'
        ? [
            { name: 'fastmock-token', purpose: 'fastmock', origin: 'https://fastmock.example.com', updatedAt: iso(NOW - 30 * DAY) },
            { name: 'gitlab-token', purpose: 'vcs', origin: 'https://gitlab.example.com', updatedAt: iso(NOW - 12 * DAY) },
            { name: 'jira-token', purpose: 'tracker', origin: 'https://issues.example.com', updatedAt: iso(NOW - 12 * DAY) },
            { name: 'slack-webhook', purpose: 'slack', origin: null, updatedAt: iso(NOW - 40 * DAY) },
            { name: 'teams-qa-channel', purpose: 'teams', origin: 'https://default0a1b2c3d.4e.environment.api.powerplatform.com', updatedAt: iso(NOW - 5 * DAY) },
            { name: 'webhook-hmac', purpose: 'webhook', origin: null, updatedAt: iso(NOW - 3 * DAY) },
          ]
        : [];
    state.secrets.set(project, list);
  }
  return list;
}

export function writeSecret(project: string, name: string, purpose: string, origin: string | null): void {
  const list = secretsOf(project).filter((s) => s.name !== name);
  list.push({ name, purpose, origin, updatedAt: new Date().toISOString() });
  list.sort((a, b) => a.name.localeCompare(b.name));
  state.secrets.set(project, list);
}

function failing(s: ResultStatus): boolean {
  return s === 'failed' || s === 'broken';
}

const previousCache = new Map<string, Map<string, ResultStatus>>();

function previousStatuses(project: string, launchId: string): Map<string, ResultStatus> | null {
  const cached = previousCache.get(launchId);
  if (cached) return cached;
  const defs = projectLaunches(project);
  const i = defs.findIndex((d) => d.launch.id === launchId);
  const prev = i >= 0 ? defs[i + 1] : undefined;
  if (!prev) return null;
  const map = new Map(launchResults(prev.launch.id).map((r) => [r.testCaseId, r.status] as const));
  previousCache.set(launchId, map);
  return map;
}

function transitionOf(project: string, r: ResultSummary): Transition | null {
  const prev = previousStatuses(project, r.launchId);
  if (!prev) return null;
  const before = prev.get(r.testCaseId);
  if (!before) return 'new';
  if (failing(before) && r.status === 'passed') return 'fixed';
  if (before === 'passed' && r.status === 'failed') return 'regressed';
  if (before === 'passed' && r.status === 'broken') return 'malfunctioned';
  return null;
}

/** Stage C fields over a stage B summary: triage state, the default defect type, transitions. */
export function analyze<T extends ResultSummary>(project: string, r: T): T {
  const t = state.triage.get(r.id);
  let defect = t && 'defect' in t ? (t.defect ?? null) : (r.defect ?? null);
  if (!defect && failing(r.status)) defect = { typeId: 'toInvestigate', group: 'toInvestigate', name: 'To investigate', source: 'rule', at: r.startedAt ?? new Date(NOW).toISOString(), by: 'fastproof' };
  if (defect?.typeId) {
    const ty = typeInfo(defect.typeId);
    if (ty) defect = { ...defect, group: ty.group, name: ty.name };
  }
  const muteUntil = t && 'muteUntil' in t ? (t.muteUntil ?? null) : (r.muteUntil ?? null);
  const muted = t?.muted ?? r.muted;
  return { ...r, defect, muted, muteUntil: muted ? muteUntil : null, transition: transitionOf(project, r), flaky: r.flakyInRun || (r.attemptCount > 1 && r.status === 'passed') };
}

export function analyzeDetail(project: string, r: ResultDetail): ResultDetail {
  const a = analyze(project, r);
  const t = state.triage.get(r.id);
  const reason = t && 'muteReason' in t ? (t.muteReason ?? null) : r.muted ? 'Cart badge race, fix in progress' : null;
  return { ...a, muteReason: a.muted ? reason : null };
}

/** The results a triage request names. */
export function triageTargets(body: TriageRequest): ResultSummary[] {
  if (body.resultIds?.length) return body.resultIds.map((id) => resultDetail(id)).filter((r): r is ResultDetail => !!r);
  if (body.launchId && body.fingerprint) return launchResults(body.launchId).filter((r) => r.fingerprint === body.fingerprint);
  return [];
}

/** Puts back the triage state of a result (the mock undo). */
export function restoreTriage(id: string, s: { defect: DefectInfo | null; muted: boolean; muteUntil: string | null }): void {
  state.triage.set(id, { defect: s.defect, muted: s.muted, muteUntil: s.muteUntil, muteReason: s.muted ? 'restored by undo' : null });
}

export function triage(project: string, body: TriageRequest): number {
  const targets = triageTargets(body);
  for (const r of targets) {
    const cur = analyze(project, r);
    const next: TriageState = { ...state.triage.get(r.id) };
    if (body.defectTypeId || body.comment !== undefined || body.issue) {
      next.defect = defectFor(body.defectTypeId ?? cur.defect?.typeId ?? 'toInvestigate', {
        comment: body.comment ?? cur.defect?.comment ?? null,
        issueUrl: body.issue?.url ?? cur.defect?.issueUrl ?? null,
        issueKey: body.issue?.key ?? cur.defect?.issueKey ?? null,
        source: body.source ?? 'user',
      });
    }
    if (body.mute) {
      next.muted = true;
      next.muteUntil = body.mute.until ?? null;
      next.muteReason = body.mute.reason;
    }
    if (body.unmute) {
      next.muted = false;
      next.muteUntil = null;
      next.muteReason = null;
    }
    state.triage.set(r.id, next);
  }
  return targets.length;
}

/** The `env` query value as the server reads it: `%00` is no environment. */
export function envParam(v: string | null): string | undefined {
  if (v === null || !v.trim()) return undefined;
  return v.trim().replace(/%(2C|25|00)/gi, (_, h: string) => (h === '00' ? '\u0000' : h === '25' ? '%' : ','));
}

/** Whether a result is in the environment of an `env` query value. */
export function inEnv(r: ResultSummary, env: string | undefined): boolean {
  return env === undefined || (resultEnv(r) ?? '\u0000') === env;
}

export function clustersOf(project: string, launchId: string, env?: string): Cluster[] {
  const results = launchResults(launchId)
    .filter((r) => inEnv(r, env))
    .map((r) => analyze(project, r));
  const defs = projectLaunches(project);
  const idx = defs.findIndex((d) => d.launch.id === launchId);
  const older = idx >= 0 ? defs.slice(idx + 1) : [];
  const groups = new Map<string, ResultSummary[]>();
  for (const r of results) {
    if (!failing(r.status) || !r.fingerprint) continue;
    const list = groups.get(r.fingerprint) ?? [];
    list.push(r);
    groups.set(r.fingerprint, list);
  }
  const out: Cluster[] = [];
  for (const [fp, list] of groups) {
    let knownSince: string | null = null;
    for (const d of older) {
      if (launchResults(d.launch.id).some((x) => x.fingerprint === fp)) knownSince = d.launch.startedAt.slice(0, 10);
    }
    const first = list[0];
    if (!first) continue;
    const types = new Set(list.map((r) => r.defect?.typeId ?? ''));
    out.push({
      fingerprint: fp,
      message: (first.message ?? '').split('\n')[0] ?? '',
      count: list.length,
      statuses: { failed: list.filter((r) => r.status === 'failed').length, broken: list.filter((r) => r.status === 'broken').length },
      sampleResultId: first.id,
      defect: types.size === 1 ? first.defect : null,
      knownSince,
      isNew: knownSince === null,
    });
  }
  return out.sort((a, b) => Number(b.isNew) - Number(a.isNew) || b.count - a.count);
}

export function similarTo(project: string, resultId: string, limit: number): SimilarFailure[] | null {
  const r = resultDetail(resultId);
  if (!r) return null;
  if (!r.fingerprint || !failing(r.status)) return [];
  const out: SimilarFailure[] = [];
  const firstLine = (r.message ?? '').split('\n')[0] ?? '';
  const words = new Set(firstLine.toLowerCase().split(/\W+/).filter((w) => w.length > 3));
  for (const d of projectLaunches(project)) {
    for (const x of launchResults(d.launch.id)) {
      if (x.id === r.id || !failing(x.status) || !x.message) continue;
      let score: number;
      if (x.fingerprint === r.fingerprint) score = x.testCaseId === r.testCaseId ? 1 : 0.94;
      else {
        const other = new Set((x.message.split('\n')[0] ?? '').toLowerCase().split(/\W+/).filter((w) => w.length > 3));
        const common = [...words].filter((w) => other.has(w)).length;
        score = words.size ? (0.75 * common) / Math.max(words.size, other.size) : 0;
      }
      if (score < 0.35) continue;
      const a = analyze(project, x);
      out.push({ resultId: x.id, launchId: d.launch.id, launchNumber: d.launch.number, name: x.name, message: (x.message.split('\n')[0] ?? '').slice(0, 300), score: Math.round(score * 100) / 100, defect: a.defect });
    }
  }
  return out.sort((a, b) => b.score - a.score || b.launchNumber - a.launchNumber).slice(0, limit);
}

export function rules(): Rule[] {
  return state.rules;
}

export class RegexError extends Error {}

function compile(src: string | undefined): RegExp | null {
  if (!src) return null;
  if (src.length > 1000) throw new RegexError('regex is longer than 1000 characters');
  try {
    return new RegExp(src, 'm');
  } catch (e) {
    throw new RegexError(e instanceof Error ? e.message : 'invalid regex');
  }
}

export function ruleMatches(launchId: string, match: RuleMatch): string[] {
  const def = launchDef(launchId);
  if (!def) return [];
  const msg = compile(match.messageRegex ?? undefined);
  const trace = compile(match.traceRegex ?? undefined);
  const out: string[] = [];
  for (const r of launchResults(launchId)) {
    if (match.statuses?.length ? !match.statuses.includes(r.status) : !failing(r.status)) continue;
    if (match.testCaseId && r.testCaseId !== match.testCaseId) continue;
    if (match.fingerprint && r.fingerprint !== match.fingerprint) continue;
    if (match.environment && def.launch.environment?.key !== match.environment) continue;
    if (msg && !msg.test(r.message ?? '')) continue;
    if (trace) {
      const d = resultDetail(r.id);
      const t = d?.attempts[d.attempts.length - 1]?.error?.trace ?? '';
      if (!trace.test(t)) continue;
    }
    out.push(r.id);
  }
  return out;
}

export function createRule(body: Partial<Rule>): Rule {
  const now = new Date().toISOString();
  const rule: Rule = {
    id: `rule-${String(++state.ruleSeq)}`,
    name: body.name ?? 'New rule',
    enabled: body.enabled ?? true,
    order: body.order ?? state.rules.length + 1,
    match: body.match ?? {},
    action: body.action ?? {},
    createdAt: now,
    updatedAt: now,
    hits: 0,
  };
  compile(rule.match.messageRegex ?? undefined);
  compile(rule.match.traceRegex ?? undefined);
  state.rules.push(rule);
  return rule;
}

export function patchRule(id: string, body: Partial<Rule>): Rule | null {
  const r = state.rules.find((x) => x.id === id);
  if (!r) return null;
  if (body.match) {
    compile(body.match.messageRegex ?? undefined);
    compile(body.match.traceRegex ?? undefined);
  }
  Object.assign(r, { ...body, id: r.id, createdAt: r.createdAt, hits: r.hits, updatedAt: new Date().toISOString() });
  return r;
}

export function deleteRule(id: string): boolean {
  const before = state.rules.length;
  state.rules = state.rules.filter((r) => r.id !== id);
  return state.rules.length < before;
}

export function applyRule(id: string, launchId: string): number | null {
  const rule = state.rules.find((r) => r.id === id);
  if (!rule) return null;
  const ids = ruleMatches(launchId, rule.match);
  for (const rid of ids) {
    const next: TriageState = { ...state.triage.get(rid) };
    if (rule.action.defectTypeId || rule.action.comment) {
      next.defect = defectFor(rule.action.defectTypeId ?? 'toInvestigate', { comment: rule.action.comment ?? null, issueUrl: rule.action.issue?.url ?? null, issueKey: rule.action.issue?.key ?? null, source: 'rule', ruleId: rule.id, by: `rule:${rule.name}` });
    }
    if (rule.action.mute) {
      next.muted = true;
      next.muteUntil = rule.action.mute.days ? iso(Date.now() + rule.action.mute.days * DAY) : null;
    }
    state.triage.set(rid, next);
  }
  rule.hits += ids.length;
  return ids.length;
}

export function flakyOf(project: string, branch?: string, environment?: string, limit = 50): FlakyTest[] {
  const defs = projectLaunches(project)
    .filter((d) => d.launch.status !== 'running')
    .filter((d) => (!branch || d.launch.vcs?.branch === branch) && (!environment || d.launch.environment?.key === environment))
    .slice(0, settingsOf(project).flaky.historyDepth * 2);
  const series = new Map<string, { r: ResultSummary; statuses: ResultStatus[]; flips: number; flakyRuns: number; lastFlakyAt: string | null }>();
  for (const d of defs) {
    for (const r of launchResults(d.launch.id)) {
      let s = series.get(r.testCaseId);
      if (!s) {
        s = { r, statuses: [], flips: 0, flakyRuns: 0, lastFlakyAt: null };
        series.set(r.testCaseId, s);
      }
      const last = s.statuses[s.statuses.length - 1];
      if (last && (last === 'passed') !== (r.status === 'passed') && r.status !== 'skipped' && last !== 'skipped') s.flips++;
      if (r.flakyInRun) {
        s.flakyRuns++;
        s.lastFlakyAt ??= d.launch.startedAt;
      } else if (last && last !== r.status) s.lastFlakyAt ??= d.launch.startedAt;
      s.statuses.push(r.status);
    }
  }
  const out: FlakyTest[] = [];
  for (const [tc, s] of series) {
    const n = s.statuses.length;
    if (n < 3) continue;
    const score = Math.min(1, (s.flips + s.flakyRuns * 0.5) / (n - 1));
    if (score < 0.15) continue;
    out.push({ testCaseId: tc, seriesKey: s.r.seriesKey, name: s.r.name, score: Math.round(score * 100) / 100, lastStatuses: s.statuses.slice(0, 10), lastFlakyAt: s.lastFlakyAt });
  }
  return out.sort((a, b) => b.score - a.score).slice(0, limit);
}

export function compare(project: string, a: string, b: string): LaunchComparison | null {
  if (!launchDef(a) || !launchDef(b)) return null;
  const ra = new Map(launchResults(a).map((r) => [r.seriesKey, analyze(project, r)] as const));
  const rb = new Map(launchResults(b).map((r) => [r.seriesKey, analyze(project, r)] as const));
  const out: LaunchComparison = { newFailures: [], fixed: [], stillFailing: [], newTests: [], missing: [], durationChanges: [], truncated: false };
  for (const [k, y] of rb) {
    const x = ra.get(k);
    if (!x) {
      out.newTests.push(y);
      continue;
    }
    if (failing(y.status) && !failing(x.status)) out.newFailures.push(y);
    else if (!failing(y.status) && failing(x.status) && y.status === 'passed') out.fixed.push(y);
    else if (failing(y.status) && failing(x.status)) out.stillFailing.push(y);
    const da = x.durationMs ?? 0;
    const db = y.durationMs ?? 0;
    if (da > 0 && db > 0 && Math.abs(db - da) >= 300 && (db / da >= 1.5 || db / da <= 0.6)) {
      out.durationChanges.push({ resultIdA: x.id, resultIdB: y.id, name: y.name, deltaMs: db - da, ratio: Math.round((db / da) * 100) / 100 });
    }
  }
  for (const [k, x] of ra) if (!rb.has(k)) out.missing.push(x);
  out.durationChanges.sort((p, q) => Math.abs(q.deltaMs) - Math.abs(p.deltaMs));
  out.durationChanges = out.durationChanges.slice(0, 50);
  return out;
}

function quantile(sorted: number[], q: number): number {
  if (!sorted.length) return 0;
  const i = Math.min(sorted.length - 1, Math.max(0, Math.round(q * (sorted.length - 1))));
  return sorted[i] ?? 0;
}

function bucketStart(t: number, bucket: 'day' | 'week'): number {
  const d = new Date(t);
  d.setUTCHours(0, 0, 0, 0);
  if (bucket === 'week') {
    const day = (d.getUTCDay() + 6) % 7;
    d.setUTCDate(d.getUTCDate() - day);
  }
  return d.getTime();
}

export function dashboard(project: string, q: { branch?: string; environment?: string; from?: string; to?: string; bucket?: string }): Dashboard {
  const bucket = q.bucket === 'week' ? 'week' : 'day';
  const to = q.to ? Date.parse(q.to) + (q.to.length <= 10 ? DAY - 1 : 0) : NOW;
  const from = q.from ? Date.parse(q.from) : NOW - 30 * DAY;
  const defs = projectLaunches(project).filter((d) => {
    const t = Date.parse(d.launch.startedAt);
    return t >= from && t <= to && (!q.branch || d.launch.vcs?.branch === q.branch) && (!q.environment || d.launch.environment?.key === q.environment) && !d.launch.debug;
  });
  const buckets = new Map<number, { passed: number; total: number; skipped: number; durations: number[]; launches: number; groups: Record<DefectGroup, number> }>();
  const failures = new Map<string, { name: string; failures: number }>();
  for (let t = bucketStart(from, bucket); t <= to; t += bucket === 'day' ? DAY : 7 * DAY) {
    buckets.set(t, { passed: 0, total: 0, skipped: 0, durations: [], launches: 0, groups: { productBug: 0, automationBug: 0, systemIssue: 0, noDefect: 0, toInvestigate: 0 } });
  }
  for (const d of defs) {
    const b = buckets.get(bucketStart(Date.parse(d.launch.startedAt), bucket));
    if (!b) continue;
    const view = launchView(d);
    b.launches++;
    b.passed += view.counts.passed;
    b.total += view.counts.total;
    b.skipped += view.counts.skipped;
    for (const r of launchResults(d.launch.id)) {
      if (r.durationMs) b.durations.push(r.durationMs);
      if (failing(r.status)) {
        const a = analyze(project, r);
        const g = a.defect?.group ?? 'toInvestigate';
        b.groups[g]++;
        const f = failures.get(r.testCaseId) ?? { name: r.name, failures: 0 };
        f.failures++;
        failures.set(r.testCaseId, f);
      }
    }
  }
  const out: Dashboard = { passRate: [], durations: [], flakyTop: flakyOf(project, q.branch, q.environment, 8), mostFailing: [], defectGroups: [], launches: [] };
  for (const [t, b] of [...buckets.entries()].sort((x, y) => x[0] - y[0])) {
    const day = new Date(t).toISOString().slice(0, 10);
    const executed = b.total - b.skipped;
    out.passRate.push({ t: day, passRate: executed > 0 ? Math.round((b.passed / executed) * 1000) / 1000 : 0, total: b.total });
    const sorted = [...b.durations].sort((x, y) => x - y);
    out.durations.push({ t: day, p50Ms: Math.round(quantile(sorted, 0.5)), p95Ms: Math.round(quantile(sorted, 0.95)) });
    out.defectGroups.push({ t: day, ...b.groups });
    out.launches.push({ t: day, count: b.launches });
  }
  out.mostFailing = [...failures.entries()]
    .map(([testCaseId, f]) => ({ testCaseId, ...f }))
    .sort((x, y) => y.failures - x.failures)
    .slice(0, 8);
  return out;
}

export function gateOf(project: string, launch: Launch): GateResult {
  const evaluatedAt = new Date().toISOString();
  if (launch.status === 'running') return { status: 'pending', results: [], launchId: launch.id, evaluatedAt };
  const gates = settingsOf(project).gates.filter((g) => g.enabled);
  const branch = launch.vcs?.branch ?? '';
  const results: GateResult['results'] = [];
  const executed = launch.counts.total - launch.counts.skipped;
  const newFailures = launchResults(launch.id).filter((r) => failing(r.status) && transitionOf(project, r) !== null && transitionOf(project, r) !== 'fixed').length;
  for (const g of gates) {
    if (g.branches?.length && !g.branches.some((p) => new RegExp(`^${p.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*')}$`).test(branch))) continue;
    for (const r of g.rules) {
      const actual =
        r.metric === 'passRate'
          ? executed > 0
            ? Math.round((launch.counts.passed / executed) * 1000) / 10
            : 0
          : r.metric === 'newFailures' || r.metric === 'newErrors'
            ? newFailures
            : r.metric === 'duration'
              ? Math.round((launch.durationMs ?? 0) / 1000)
              : r.metric === 'toInvestigate'
                ? launch.counts.failed + launch.counts.broken
                : r.metric === 'total'
                  ? launch.counts.total - launch.counts.skipped
                  : launch.counts[r.metric];
      const ok = r.op === 'gt' ? actual > r.value : r.op === 'gte' ? actual >= r.value : r.op === 'lt' ? actual < r.value : actual <= r.value;
      results.push({ gate: g.name, rule: r, actual, expected: r.value, ok });
    }
  }
  return { status: results.every((r) => r.ok) ? 'passed' : 'failed', results, launchId: launch.id, evaluatedAt };
}

export function deliveries(project: string): Delivery[] {
  const notes = settingsOf(project).notifications;
  if (!notes.length) return [];
  const out: Delivery[] = [];
  const launches = projectLaunches(project).filter((d) => d.launch.status !== 'running');
  for (let i = 0; i < 36; i++) {
    const n = notes[i % notes.length];
    const d = launches[Math.floor(i / notes.length)];
    if (!n || !d) break;
    const h = hash(`${n.id}:${d.launch.id}`);
    const status: Delivery['status'] = i === 1 ? 'pending' : h % 9 === 0 ? 'failed' : 'delivered';
    const event = n.on[h % n.on.length] ?? 'launchFinished';
    const created = Date.parse(d.launch.finishedAt ?? d.launch.startedAt) + 5000;
    out.push({
      id: `dlv-${String(i)}`,
      notificationId: n.id,
      kind: n.kind,
      events: [event],
      status,
      maxAttempts: 5,
      nextAttemptAt: status === 'pending' ? iso(created + 120_000) : null,
      attempts: status === 'delivered' ? 1 + (h % 3 === 0 ? 1 : 0) : status === 'failed' ? 5 : 2,
      responseStatus: status === 'delivered' ? (n.kind === 'email' ? null : 200) : status === 'failed' ? 503 : 502,
      lastError: status === 'delivered' ? null : status === 'failed' ? 'HTTP 503 Service Unavailable after 5 attempts' : 'HTTP 502 Bad Gateway, retrying in 2m',
      createdAt: iso(created),
      deliveredAt: status === 'delivered' ? iso(created + 800) : null,
      launchId: d.launch.id,
    });
  }
  return out;
}

export function mockInteractions(project: string, resultId: string, attempt: number): MockInteractions | 'notConfigured' | null {
  const r = resultDetail(resultId);
  if (!r) return null;
  if (!settingsOf(project).fastmock) return 'notConfigured';
  const a = r.attempts.find((x) => x.no === attempt) ?? r.attempts[r.attempts.length - 1];
  const start = a?.startedAt ? Date.parse(a.startedAt) : NOW - HOUR;
  const kind = testKind(resultId);
  const failingAttempt = a ? failing(a.status) : false;
  const items: MockInteractions['items'] = [
    { at: iso(start + 220), method: 'GET', url: '/catalog/products/running-shoe', status: 200, matchedStub: 'catalog-product-by-slug', durationMs: 12 },
    { at: iso(start + 640), method: 'POST', url: '/cart/items', status: 201, matchedStub: 'cart-add-item', durationMs: 18 },
    { at: iso(start + 910), method: 'GET', url: '/cart', status: 200, matchedStub: 'cart-get', durationMs: 9 },
    failingAttempt
      ? { at: iso(start + 1480), method: 'POST', url: '/payments', status: 502, matchedStub: 'payments-gateway-down', fault: 'injected 502 (scenario payments-outage)', durationMs: 1210 }
      : { at: iso(start + 1480), method: 'POST', url: '/payments', status: 201, matchedStub: 'payments-authorize', durationMs: 402 },
    { at: iso(start + 1720), method: 'GET', url: '/orders/SO-2026-118842', status: 404, matchedStub: null, nearMiss: true, durationMs: 3 },
  ];
  return { source: kind === 'ui' ? 'traceId' : 'timeWindow', confidence: kind === 'ui' ? 1 : 0.62, items: kind === 'ui' ? items : items.slice(0, 3), attempt: a?.no ?? attempt, truncated: false };
}

export function setImportant(id: string, v: boolean): void {
  state.important.set(id, v);
}

export function isImportant(id: string, fallback: boolean): boolean {
  return state.important.get(id) ?? fallback;
}

export function createPlan(project: string, plan: TestPlanCreate): string {
  const id = `plan-${String(state.plans.size + 1)}-${(hash(JSON.stringify(plan)) % 100000).toString(36)}`;
  state.plans.set(id, { project, plan });
  return id;
}

export function planJson(id: string): { version: string; tests: { id?: string; selector: string }[] } | null {
  const p = state.plans.get(id);
  if (!p) return null;
  const { plan } = p;
  let results: ResultSummary[] = plan.fromLaunchId ? launchResults(plan.fromLaunchId) : [];
  if (plan.statuses?.length) results = results.filter((r) => plan.statuses?.includes(r.status));
  if (plan.testCaseIds?.length) results = results.filter((r) => plan.testCaseIds?.includes(r.testCaseId));
  return { version: '1.0', tests: results.map((r) => ({ id: r.testCaseId, selector: r.fullName })) };
}

const reruns: Rerun[] = [];

export function rerunsOf(launchId: string): RerunList {
  const settings = settingsOf(launchDef(launchId)?.project ?? 'web-shop');
  const started = reruns.find((r) => r.launchId === launchId) ?? null;
  return { configured: !!settings.rerun, provider: settings.rerun?.provider ?? null, items: reruns.filter((r) => r.sourceLaunchId === launchId), rerunOf: started };
}

export function createRerun(launchId: string, body: RerunRequest): Rerun {
  const def = launchDef(launchId);
  const results = launchResults(launchId);
  const picked = body.mode === 'selected' ? results.filter((r) => body.resultIds?.includes(r.id)) : results.filter((r) => (body.statuses ?? ['failed', 'broken']).includes(r.status));
  const id = `rerun-${String(reruns.length + 1)}`;
  const r: Rerun = {
    id,
    sourceLaunchId: launchId,
    sourceLaunchNumber: def?.launch.number ?? 0,
    externalId: id,
    planId: `plan-${id}`,
    provider: settingsOf(def?.project ?? 'web-shop').rerun?.provider ?? 'github',
    mode: body.mode,
    testCount: picked.length,
    status: 'dispatched',
    error: null,
    ciRef: null,
    requestedBy: 'alex@example.com',
    createdAt: new Date().toISOString(),
    dispatchedAt: new Date().toISOString(),
    launchId: null,
    launchNumber: null,
  };
  reruns.unshift(r);
  return r;
}

export function jobs(): ScheduledJob[] {
  const j = (name: string, schedule: string, everyMs: number, extra: Partial<ScheduledJob> = {}): ScheduledJob => {
    const last = NOW - (hash(name) % everyMs);
    return {
      name,
      schedule,
      nextRunAt: iso(last + everyMs),
      lastStartedAt: iso(last),
      lastFinishedAt: iso(last + 1200 + (hash(name) % 9000)),
      lastDurationMs: 1200 + (hash(name) % 9000),
      lastStatus: 'ok',
      lastError: null,
      runCount: 200 + (hash(name) % 4000),
      failureCount: hash(name) % 4,
      leaseOwner: null,
      leaseUntil: null,
      ...extra,
    };
  };
  return [
    j('history.rollup', 'every 1h', HOUR),
    j('retention.apply', 'daily at 03:30 UTC', DAY, { lastStatus: 'failed', lastError: 'blob store: 3 deletions failed (will retry)', failureCount: 7 }),
    j('uploads.reap', 'every 5m', 5 * MIN),
    j('launches.autofinish', 'every 1m', MIN, { leaseOwner: 'fastproof-6d9f7c-xk2lp', leaseUntil: iso(NOW + 40_000) }),
    j('partitions.maintain', 'daily at 01:00 UTC', DAY),
    j('notifications.outbox', 'every 10s', 10_000),
  ];
}

const LEVEL_RANK: Record<LogLevel, number> = { trace: 0, debug: 1, info: 2, warn: 3, error: 4 };

const MESSAGES: [LogLevel, LogLineWire['stream'], (i: number) => string][] = [
  ['info', 'stdout', (i) => `${ESC}[90m[worker-${String(i % 4)}]${ESC}[0m navigate https://shop.example.com/p/item-${String(i % 97)}`],
  ['debug', 'log', (i) => `locator.click getByRole('button', { name: 'Add to cart' }) attempt ${String((i % 3) + 1)}`],
  ['info', 'stdout', (i) => `GET https://api.example.com/cart ${ESC}[32m200${ESC}[0m ${String(40 + (i % 160))} ms`],
  ['trace', 'log', (i) => `frame tick ${String(i)} (layout ${String(i % 13)} ms)`],
  ['warn', 'stderr', (i) => `${ESC}[33mwarning${ESC}[0m slow response from https://api.example.com/search: ${(1.2 + (i % 20) / 10).toFixed(1)} s`],
  ['info', 'step', (i) => `step "Fill the payment form" ${i % 2 ? 'started' : 'passed'}`],
  ['error', 'stderr', (i) => `${ESC}[1;31mERROR${ESC}[0m POST https://api.example.com/payments ${ESC}[41;97m 502 ${ESC}[0m (attempt ${String((i % 3) + 1)}/3)`],
  ['info', 'stdout', (i) => `${ESC}[32m✓${ESC}[0m assertion ${String(i % 50)} passed`],
];

export function bigLogResult(): string | null {
  const rich = projectLaunches('web-shop').find((d) => d.launch.number === 41);
  const r = rich ? launchResults(rich.launch.id).find((x) => x.name === 'pays with saved card') : undefined;
  return r?.id ?? null;
}

export const BIG_LOG_LINES = 120_000;

function linesOf(resultId: string, attempt: number, count: number, start: number): LogLineWire[] {
  const out: LogLineWire[] = [];
  const seed = hash(resultId);
  for (let i = 0; i < count; i++) {
    const m = MESSAGES[(seed + i * 7 + (i % 5)) % MESSAGES.length] ?? MESSAGES[0];
    if (!m) continue;
    out.push({ resultId, attempt, seq: i + 1, ts: start + i * 35, level: m[0], stream: m[1], message: m[2](i), step: m[1] === 'step' ? 'Pay for the order' : null });
  }
  return out;
}

const storedCache = new Map<string, LogLineWire[]>();

export function storedLogs(launchId: string, resultId: string | null, attempt: number | null): LogLineWire[] {
  const key = `${launchId}:${resultId ?? '*'}:${String(attempt ?? '*')}`;
  const cached = storedCache.get(key);
  if (cached) return cached;
  // Like the server: a result id stands for all its attempts' client ids, a client id for one attempt.
  const results = launchResults(launchId).filter((r) => !resultId || r.id === resultId || (r.clientIds ?? []).includes(resultId));
  const big = bigLogResult();
  let out: LogLineWire[] = [];
  for (const r of results) {
    if (r.status === 'skipped') continue;
    const one = resultId && resultId !== r.id ? (r.clientIds ?? []).indexOf(resultId) : -1;
    const attempts = one >= 0 ? [one + 1] : attempt !== null ? [attempt + 1] : Array.from({ length: r.attemptCount }, (_, i) => i + 1);
    const d = resultDetail(r.id);
    for (const a of attempts) {
      const at = d?.attempts.find((x) => x.no === a);
      const start = at?.startedAt ? Date.parse(at.startedAt) : NOW;
      const count = r.id === big && a === r.attemptCount ? BIG_LOG_LINES : 24 + (hash(`${r.id}:${String(a)}`) % 80);
      out = out.concat(linesOf(clientIdOf(r.id, a - 1), a - 1, count, start));
    }
  }
  if (!resultId) out.sort((a, b) => a.ts - b.ts || a.seq - b.seq);
  if (storedCache.size > 20) storedCache.clear();
  storedCache.set(key, out);
  return out;
}

export function filterLogs(lines: LogLineWire[], level: string | null, q: string | null): LogLineWire[] {
  const min = level && level in LEVEL_RANK ? LEVEL_RANK[level as LogLevel] : 0;
  const needle = q?.toLowerCase() ?? '';
  if (!min && !needle) return lines;
  return lines.filter((l) => LEVEL_RANK[l.level] >= min && (!needle || l.message.toLowerCase().includes(needle)));
}

let liveSeq = 0;

/** A batch of live lines for the running launch. */
export function liveBatch(launchId: string, resultId: string | null, tick: number): LogLineWire[] {
  const results = launchResults(launchId).slice(-12);
  const pool = resultId ? results.filter((r) => r.id === resultId || (r.clientIds ?? []).includes(resultId)) : results;
  const ids = pool.length ? pool.map((r) => clientIdOf(r.id, 0)) : resultId ? [resultId] : ['running-result'];
  const n = 4 + (tick % 9) * 3;
  const out: LogLineWire[] = [];
  for (let i = 0; i < n; i++) {
    liveSeq++;
    const id = ids[(tick + i) % ids.length] ?? 'running-result';
    const m = MESSAGES[(tick * 3 + i) % MESSAGES.length] ?? MESSAGES[0];
    if (!m) continue;
    out.push({ resultId: id, attempt: 0, seq: liveSeq, ts: Date.now(), level: m[0], stream: m[1], message: m[2](liveSeq), step: null });
  }
  return out;
}

export { LOG_LEVELS };
