import type { CaseStep, CaseStepHtml } from '../api/tm';
import type { SharedPin, SharedStep, SharedStepBrief, SharedStepSnapshot, SharedStepVersionSummary } from '../api/sharedSteps';
import { renderMarkdown } from './tmMarkdown';

export interface MockVersion extends SharedStepVersionSummary {
  snapshot: SharedStepSnapshot;
}

export interface MockShared {
  id: string;
  number: number;
  name: string;
  description: string;
  steps: CaseStep[];
  version: number;
  revision: number;
  archivedAt: string | null;
  aiUnreviewed: boolean;
  versions: MockVersion[];
  usedBy: number[];
  openRuns: number;
  createdBy: string;
  createdAt: string;
  updatedBy: string;
  updatedAt: string;
}

let seq = 0;


export function sid(): string {
  seq += 1;
  return `s_mock${String(seq).padStart(6, '0')}`;
}

export function step(action: string, expected = '', children: CaseStep[] = []): CaseStep {
  return { id: sid(), action, expected, data: '', attachments: [], steps: children };
}

export function uuid(n: number): string {
  return `0190a1b2-c3d4-7e5f-8a9b-${String(n).padStart(12, '0')}`;
}

function seed(): MockShared[] {
  const at = '2026-11-20T09:00:00Z';
  const later = '2026-11-22T14:30:00Z';
  const login1: CaseStep[] = [step('Open the **login** page', 'The sign-in form is shown'), step('Sign in with the test account', 'The dashboard opens')];
  const login2: CaseStep[] = [
    step('Open the **login** page', 'The sign-in form is shown'),
    step('Sign in', '', [step('Type the user name'), step('Type the password and press Enter', 'The dashboard opens')]),
  ];
  const list: MockShared[] = [
    {
      id: uuid(1),
      number: 1,
      name: 'Log in as a customer',
      description: 'Signs in with the **customer** test account.',
      steps: login2,
      version: 2,
      revision: 2,
      archivedAt: null,
      aiUnreviewed: false,
      versions: [
        { version: 2, source: 'ui', changed: ['steps'], createdBy: 'anna', createdAt: later, snapshot: { name: 'Log in as a customer', description: 'Signs in with the **customer** test account.', steps: login2 } },
        { version: 1, source: 'ui', changed: ['name', 'description', 'steps'], createdBy: 'anna', createdAt: at, snapshot: { name: 'Log in as a customer', description: 'Signs in with the **customer** test account.', steps: login1 } },
      ],
      usedBy: [1, 2, 3],
      openRuns: 1,
      createdBy: 'anna',
      createdAt: at,
      updatedBy: 'anna',
      updatedAt: later,
    },
    {
      id: uuid(2),
      number: 2,
      name: 'Add an item to the cart',
      description: '',
      steps: [step('Open a product page'), step('Press **Add to cart**', 'The cart counter grows by one')],
      version: 1,
      revision: 1,
      archivedAt: null,
      aiUnreviewed: false,
      versions: [],
      usedBy: [2],
      openRuns: 0,
      createdBy: 'boris',
      createdAt: at,
      updatedBy: 'boris',
      updatedAt: at,
    },
    {
      id: uuid(3),
      number: 3,
      name: 'Legacy login (SSO off)',
      description: 'Kept for old cases.',
      steps: [step('Sign in with a password')],
      version: 1,
      revision: 3,
      archivedAt: '2026-11-21T10:00:00Z',
      aiUnreviewed: false,
      versions: [],
      usedBy: [],
      openRuns: 0,
      createdBy: 'anna',
      createdAt: at,
      updatedBy: 'anna',
      updatedAt: at,
    },
  ];
  for (const s of list) {
    if (s.versions.length === 0) s.versions = [{ version: 1, source: 'ui', changed: ['name', 'description', 'steps'], createdBy: s.createdBy, createdAt: s.createdAt, snapshot: { name: s.name, description: s.description, steps: s.steps } }];
  }
  return list;
}

let all: MockShared[] = seed();

export function sharedAll(): MockShared[] {
  return all;
}

export function setSharedAll(next: MockShared[]): void {
  all = next;
}

export function resetSharedSteps(): void {
  all = seed();
  seq = 0;
}

export function htmlSteps(steps: CaseStep[], project: string): CaseStepHtml[] {
  return steps.map((s) => ({ id: s.id, action: renderMarkdown(s.action, project), expected: renderMarkdown(s.expected, project), data: renderMarkdown(s.data, project), steps: htmlSteps(s.steps, project) }));
}

export function view(s: MockShared, project: string): SharedStep {
  return {
    id: s.id,
    number: s.number,
    name: s.name,
    description: s.description,
    steps: s.steps,
    html: { description: renderMarkdown(s.description, project), steps: htmlSteps(s.steps, project) },
    version: s.version,
    approvedVersion: null,
    reviewState: null,
    origin: 'ui',
    aiUnreviewed: s.aiUnreviewed,
    usage: { cases: s.usedBy.length, openRuns: s.openRuns },
    revision: s.revision,
    createdBy: s.createdBy,
    createdAt: s.createdAt,
    updatedBy: s.updatedBy,
    updatedAt: s.updatedAt,
    archivedAt: s.archivedAt,
  };
}

/** The runnable version of a shared step in the mock: its head. */
function currentOf(id: string): MockShared | undefined {
  return all.find((s) => s.id === id);
}

function stepsAt(s: MockShared, version: number): CaseStep[] {
  return structuredClone(s.versions.find((v) => v.version === version)?.snapshot.steps ?? s.steps);
}

/** The shared steps a case body references, as `Case.sharedSteps`. */
export function sharedBriefsFor(steps: CaseStep[], project: string): SharedStepBrief[] {
  const out: SharedStepBrief[] = [];
  for (const st of steps) {
    const s = st.shared ? currentOf(st.shared.id) : undefined;
    if (!s || out.some((b) => b.id === s.id)) continue;
    out.push({ id: s.id, number: s.number, name: s.name, version: s.version, archived: !!s.archivedAt, steps: s.steps, stepsHtml: htmlSteps(s.steps, project) });
  }
  return out;
}

/** What a run item holds for a case body: each `shared` step carries the current version and its steps. */
export function expandSharedForRun(steps: CaseStep[]): CaseStep[] {
  return steps.map((st) => {
    const s = st.shared ? currentOf(st.shared.id) : undefined;
    if (!st.shared || !s) return st;
    return { ...st, shared: { ...st.shared, version: s.version }, steps: stepsAt(s, s.version) };
  });
}

/** The pins of a run item, read from its expanded steps. */
export function sharedPinsOf(steps: CaseStep[]): SharedPin[] {
  const out: SharedPin[] = [];
  for (const st of steps) {
    const ref = st.shared;
    const s = ref ? currentOf(ref.id) : undefined;
    if (!ref || !s || out.some((p) => p.id === ref.id)) continue;
    out.push({ id: ref.id, number: ref.number, name: stepsAtName(s, ref.version ?? s.version), version: ref.version ?? s.version, current: s.version });
  }
  return out;
}

function stepsAtName(s: MockShared, version: number): string {
  return s.versions.find((v) => v.version === version)?.snapshot.name ?? s.name;
}
