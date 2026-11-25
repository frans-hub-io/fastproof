import type {
  CaseBody,
  CaseFieldDef,
  CaseIdentity,
  CaseKey,
  CaseOrigin,
  CaseSnapshot,
  CaseStatus,
  CaseStep,
  CaseSuggestion,
  CaseVersion,
  FieldValue,
  ImportPreview,
  LastResult,
  RunStatus,
  RunStepResult,
  StepStatus,
  TestIdentity,
  TestPlan,
  RunItemExecution,
} from '../api/tm';
import { CATALOG, launchResults, launchView, NOW, projectLaunches } from './fixtures';
import { expandSharedForRun } from './sharedStepsState';

const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

export const TM_PROJECT = 'web-shop';
export const ME = 'alex@example.com';
export const PEOPLE = [ME, 'maria@example.com', 'oleh@example.com', 'sam@example.com'];
export const AI_AGENT = 'fastproof-cases';

function iso(t: number): string {
  return new Date(t).toISOString();
}

function hash(text: string, seed = 0x811c9dc5): number {
  let h = seed >>> 0;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h >>> 0;
}

export function tmId(text: string): string {
  let out = '';
  for (let i = 0; out.length < 32; i++) out += hash(text, 0x811c9dc5 + i * 7919).toString(16).padStart(8, '0');
  return `${out.slice(0, 8)}-${out.slice(8, 12)}-7${out.slice(13, 16)}-8${out.slice(17, 20)}-${out.slice(20, 32)}`;
}

function rng(seed: number): () => number {
  let s = seed >>> 0 || 1;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export interface MockFolder {
  id: string;
  parentId: string | null;
  name: string;
  position: number;
}

export interface MockCase {
  id: string;
  number: number;
  name: string;
  status: CaseStatus;
  origin: CaseOrigin;
  aiUnreviewed: boolean;
  folderId: string | null;
  body: CaseBody;
  fields: Record<string, FieldValue>;
  tags: string[];
  identities: CaseIdentity[];
  keys: CaseKey[];
  revision: number;
  version: number;
  createdBy: string | null;
  createdAt: string;
  updatedBy: string | null;
  updatedAt: string;
  archivedAt: string | null;
  versions: CaseVersion[];
  lastResult?: LastResult;
  attachments: string[];
}

export interface MockPlanItem {
  caseNumber: number;
  assignee: string | null;
  viaFilter: boolean;
  position: number;
}

export interface MockPlan extends TestPlan {
  items: MockPlanItem[];
}

export interface MockRunItem {
  id: string;
  launchId: string;
  caseNumber: number;
  caseVersion: number;
  name: string;
  folderPath: string[];
  status: RunStatus;
  assignee: string | null;
  executionNo: number;
  updatedAt: string;
  preconditions: string;
  steps: CaseStep[];
  stepResults: RunStepResult[];
  comment: string;
  defect: unknown;
  executedBy: string | null;
  startedAt: string | null;
  finishedAt: string | null;
  revision: number;
  resultId: string | null;
}

export interface MockManualLaunch {
  launchId: string;
  planId: string | null;
  state: 'open' | 'closed';
}

// Folders

interface FolderSpec {
  name: string;
  children?: FolderSpec[];
  cases?: string[];
}

const TREE: FolderSpec[] = [
  {
    name: 'Storefront',
    children: [
      {
        name: 'Catalog',
        children: [
          { name: 'Search', cases: ['Search by product name', 'Search suggests while typing', 'Search corrects a typo', 'Search by SKU', 'Empty search shows popular items', 'Search inside a category', 'Search keeps recent queries', 'Search clears recent queries', 'Search handles special characters', 'Search ranks exact matches first', 'Search results paginate', 'Search by barcode from the camera', 'Search in Ukrainian', 'Voice search opens the microphone prompt', 'No results page offers help'] },
          { name: 'Filters', cases: ['Filter by price range', 'Filter by brand', 'Filter by size', 'Filter by colour', 'Combine three filters', 'Filters stay in the URL', 'Reset all filters', 'Filter counts update', 'Filter by rating', 'Filter by availability in a store', 'Sort by price after filtering', 'Filter chips can be removed one by one'] },
          { name: 'Product page', cases: ['Product page shows price and stock', 'Switch a product variant', 'Zoom product images', 'Read reviews', 'Write a review', 'Open the size guide', 'Compare two products', 'Related products carousel', 'Sale badge shows the discount', 'Price history chart', 'Add to the wish list', 'Share a product link'] },
        ],
      },
      {
        name: 'Cart',
        cases: ['Add a product to the cart', 'Remove a product from the cart', 'Change the quantity', 'Cart survives a new session', 'Guest cart merges on sign-in', 'Quantity is limited to stock', 'Bundle price applies', 'Empty the cart', 'Cart badge shows the count', 'Save an item for later', 'Price change warning', 'Free shipping threshold message', 'Restore a removed item', 'Notes per cart item', 'Share the cart'],
      },
      {
        name: 'Checkout',
        children: [
          { name: 'Payment', cases: ['Pay with a saved card', 'Apply a discount code at checkout', 'Pay with a new card', 'Expired card is rejected', '3-D Secure challenge completes', 'Declined card can be retried', 'Pay with a gift card', 'Gift card and card together', 'Cancel a pending payment', 'Payment page keeps the cart after failure', 'Receipt page shows the order', 'Download the invoice PDF', 'Billing address validation', 'Pay with Apple Pay on Safari', 'Pay with Google Pay on Android'] },
          { name: 'Shipping', cases: ['Choose express delivery', 'Heavy items add shipping cost', 'Order splits by warehouse', 'Delivery address is remembered', 'Taxes per region', 'Pick up in a store', 'Delivery time slot', 'Change the address after ordering', 'Shipping to another country', 'Shipping estimate on the cart page'] },
          { name: 'Order confirmation', cases: ['Confirmation email arrives', 'Order number is shown', 'Confirmation page links to tracking', 'SMS notification for the order', 'Order appears in the account'] },
        ],
      },
      {
        name: 'Account',
        children: [
          { name: 'Sign-in', cases: ['Sign up with email', 'Sign in with a password', 'Sign out', 'Reset a forgotten password', 'Enable two-factor sign-in', 'Account locks after failed attempts', 'Sign in with SSO', 'Session survives a reload', 'Idle session expires', 'Remember me keeps the session'] },
          { name: 'Profile', cases: ['Change the email address', 'Manage saved cards', 'Manage addresses', 'Download personal data', 'Delete the account', 'Newsletter preferences', 'Loyalty points balance', 'Redeem loyalty points', 'Past orders list', 'Reorder a past order'] },
        ],
      },
    ],
  },
  {
    name: 'Shop API',
    children: [
      { name: 'Orders', cases: ['Create an order through the API', 'Reject an empty order', 'List orders of a customer', 'Cancel an order', 'Refuse cancel after shipping', 'Idempotency key prevents duplicates', 'Order created event is emitted', 'Order totals with coupon'] },
      { name: 'Inventory', cases: ['Reserve stock', 'Release a reservation', 'Reservation expires', 'Low stock report', 'Move stock between warehouses', 'Backorder handling', 'Restock notification', 'Inventory export'] },
    ],
  },
  {
    name: 'Mobile web',
    cases: ['Navigation drawer on a phone', 'Product page at 360 px', 'Checkout on a phone', 'Camera barcode search', 'Pinch to zoom images', 'Offline banner', 'Add to home screen', 'Dark theme follows the system'],
  },
  { name: 'Automated' },
];

// Fields

export function initialFields(): CaseFieldDef[] {
  let pos = 0;
  const f = (key: string, name: string, type: CaseFieldDef['type'], o: Partial<CaseFieldDef> = {}): CaseFieldDef => ({
    id: tmId(`field:${key}`),
    key,
    name,
    type,
    options: [],
    required: false,
    defaultValue: null,
    sourceLabel: null,
    builtIn: false,
    position: pos++,
    archived: false,
    ...o,
  });
  const opts = (values: string[], colors: string[] = []) => values.map((value, i) => ({ value, archived: false, ...(colors[i] ? { color: colors[i] } : {}) }));
  return [
    f('priority', 'Priority', 'select', { options: opts(['Critical', 'High', 'Medium', 'Low'], ['#e03131', '#f08c00', '#1971c2', '#868e96']), defaultValue: 'Medium', required: true }),
    f('severity', 'Severity', 'select', { options: opts(['blocker', 'critical', 'normal', 'minor', 'trivial']), sourceLabel: 'severity', builtIn: true, defaultValue: 'normal' }),
    f('owner', 'Owner', 'text', { sourceLabel: 'owner', builtIn: true }),
    f('component', 'Component', 'multiselect', { options: opts(['Catalog', 'Search', 'Cart', 'Checkout', 'Payments', 'Shipping', 'Account', 'Orders API', 'Inventory API', 'Mobile']), sourceLabel: 'component', builtIn: true }),
    f('layer', 'Layer', 'select', { options: opts(['UI', 'API', 'Unit']), sourceLabel: 'layer', builtIn: true }),
    f('epic', 'Epic', 'text', { sourceLabel: 'epic', builtIn: true }),
    f('feature', 'Feature', 'text', { sourceLabel: 'feature', builtIn: true }),
    f('story', 'Story', 'text', { sourceLabel: 'story', builtIn: true }),
    f('estimate', 'Estimate, minutes', 'number'),
    f('reviewDue', 'Review due', 'date'),
    f('platform', 'Platform', 'multiselect', { options: opts(['Desktop', 'Mobile web', 'iOS', 'Android']) }),
  ];
}

/** Fields whose source policy is `code` (settings default) on automated cases. */
export const CODE_FIELDS = ['name', 'description', 'links', 'tags', 'epic', 'feature', 'story', 'suite', 'severity', 'owner', 'layer', 'component'];

// Steps

const ACTIONS: [string, string, string][] = [
  ['Open the storefront at `https://shop.example.com`', 'The home page loads in under **2 s**', ''],
  ['Sign in as the test customer', 'The account menu shows the customer name', 'email: `buyer@example.com`'],
  ['Open the product page of *Trail running shoes*', 'Price, sizes and stock are shown', 'SKU: `TRS-42`'],
  ['Choose size 42 and press **Add to cart**', 'The cart badge shows 1', ''],
  ['Open the cart', 'The item, quantity and total are listed', ''],
  ['Press **Checkout**', 'The delivery step opens with the saved address', ''],
  ['Choose standard delivery', 'Delivery cost and date are shown', ''],
  ['Continue to payment', 'Saved cards are listed, the default one is selected', ''],
  ['Confirm the order', 'The confirmation page shows the order number', ''],
  ['Check the mailbox', 'A confirmation email arrives within 1 minute', 'mailbox: `buyer@example.com`'],
];

function step(seed: string, action: string, expected: string, data = '', steps: CaseStep[] = []): CaseStep {
  return { id: `s_${tmId(seed).replace(/-/g, '').slice(0, 20)}`, action, expected, data, attachments: [], steps };
}

function genSteps(seed: string, r: () => number): CaseStep[] {
  const n = 3 + Math.floor(r() * 4);
  const start = Math.floor(r() * (ACTIONS.length - n));
  const out: CaseStep[] = [];
  for (let i = 0; i < n; i++) {
    const a = ACTIONS[start + i] ?? ACTIONS[0];
    if (!a) continue;
    const nested = i === 1 && r() < 0.35;
    out.push(
      step(
        `${seed}:${String(i)}`,
        a[0],
        nested ? '' : a[1],
        a[2],
        nested ? [step(`${seed}:${String(i)}:a`, 'Enter the email', 'The field accepts it', 'email: `buyer@example.com`'), step(`${seed}:${String(i)}:b`, 'Enter the password and press **Sign in**', 'The account menu shows the customer name', 'password: from the vault entry *shop-e2e*')] : [],
      ),
    );
  }
  return out;
}

function body(description: string, preconditions: string, steps: CaseStep[], links: CaseBody['links'] = []): CaseBody {
  return { description, preconditions, steps, links };
}

export function snapshotOf(c: Pick<MockCase, 'name' | 'status' | 'folderId' | 'body' | 'fields' | 'tags'>): CaseSnapshot {
  return structuredClone({ name: c.name, status: c.status, folderId: c.folderId, body: c.body, fields: c.fields, tags: c.tags });
}

export interface TmState {
  seq: number;
  folders: MockFolder[];
  fields: CaseFieldDef[];
  cases: Map<number, MockCase>;
  suggestions: CaseSuggestion[];
  identities: TestIdentity[];
  plans: MockPlan[];
  runItems: MockRunItem[];
  manual: Map<string, MockManualLaunch>;
  createdLaunches: Map<string, ReturnType<typeof launchView>>;
  /** Earlier executions of run items, kept by rerun. */
  executions: (RunItemExecution & { itemId: string })[];
  attachments: Map<string, { name: string; contentType: string; body: ArrayBuffer }>;
  jobs: Map<string, import('../api/tm').TmJob & { rows?: string[][]; headers?: string[]; options?: import('../api/tm').ImportOptions; tick: number }>;
}

const FOLDER_COMPONENT: Record<string, string> = {
  Search: 'Search',
  Filters: 'Catalog',
  'Product page': 'Catalog',
  Cart: 'Cart',
  Payment: 'Payments',
  Shipping: 'Shipping',
  'Order confirmation': 'Checkout',
  'Sign-in': 'Account',
  Profile: 'Account',
  Orders: 'Orders API',
  Inventory: 'Inventory API',
  'Mobile web': 'Mobile',
};

export function manualLaunchId(): string {
  const def = projectLaunches(TM_PROJECT).find((d) => d.launch.number === 38);
  return def?.launch.id ?? '';
}

export function buildTmState(): TmState {
  const r = rng(2026);
  const folders: MockFolder[] = [];
  const cases = new Map<number, MockCase>();
  let seq = 0;
  const names = new Map<string, string>();

  const addCase = (c: Omit<MockCase, 'number' | 'id' | 'versions' | 'revision' | 'version' | 'attachments'> & { versions?: CaseVersion[] }): MockCase => {
    seq++;
    const mc: MockCase = { ...c, number: seq, id: tmId(`case:${String(seq)}`), revision: 1, version: 1, versions: [], attachments: [] };
    mc.versions = c.versions ?? [{ version: 1, source: c.origin === 'code' ? 'code' : c.origin === 'import' ? 'import' : c.origin === 'ai' ? 'ai' : 'ui', changed: ['created'], createdBy: c.createdBy ?? 'system', createdAt: c.createdAt, snapshot: snapshotOf(mc) }];
    mc.version = mc.versions.length;
    mc.revision = mc.versions.length;
    cases.set(seq, mc);
    return mc;
  };

  const walk = (list: FolderSpec[], parentId: string | null, path: string[]) => {
    list.forEach((spec, position) => {
      const id = tmId(`folder:${[...path, spec.name].join('/')}`);
      folders.push({ id, parentId, name: spec.name, position });
      names.set(spec.name, id);
      for (const name of spec.cases ?? []) {
        const seed = `${spec.name}:${name}`;
        const created = NOW - (30 + Math.floor(r() * 300)) * DAY;
        const updated = created + Math.floor(r() * 25) * DAY;
        const priority = ['Critical', 'High', 'Medium', 'Medium', 'Low'][Math.floor(r() * 5)] ?? 'Medium';
        const owner = PEOPLE[Math.floor(r() * PEOPLE.length)] ?? ME;
        const statusRoll = r();
        addCase({
          name,
          status: statusRoll < 0.08 ? 'draft' : statusRoll < 0.14 ? 'outdated' : 'active',
          origin: r() < 0.2 ? 'import' : 'ui',
          aiUnreviewed: false,
          folderId: id,
          body: body(`Checks that the customer can **${name.toLowerCase()}**.`, r() < 0.6 ? '- A customer account with a saved address\n- At least one product in stock' : '', genSteps(seed, r)),
          fields: {
            priority,
            severity: priority === 'Critical' ? 'critical' : 'normal',
            owner,
            component: [FOLDER_COMPONENT[spec.name] ?? 'Catalog'],
            layer: path[0] === 'Shop API' ? 'API' : 'UI',
            estimate: 5 + Math.floor(r() * 25),
            platform: spec.name === 'Mobile web' ? ['Mobile web'] : r() < 0.3 ? ['Desktop', 'Mobile web'] : ['Desktop'],
          },
          tags: r() < 0.4 ? ['regression'] : r() < 0.6 ? ['smoke', 'regression'] : [],
          identities: [],
          keys: r() < 0.2 ? [{ kind: 'testrail', value: `C${String(1000 + seq)}` }] : [],
          createdBy: owner,
          createdAt: iso(created),
          updatedBy: owner,
          updatedAt: iso(updated),
          archivedAt: null,
        });
      }
      if (spec.children) walk(spec.children, id, [...path, spec.name]);
    });
  };
  walk(TREE, null, []);

  // Automated cases from the code catalog, in "Automated/<suite>" folders (folderFrom = suite).
  const automatedRoot = names.get('Automated') ?? null;
  const rich = launchResults(projectLaunches(TM_PROJECT)[1]?.launch.id ?? '');
  const byTestCase = new Map(rich.map((x) => [x.testCaseId, x] as const));
  const suiteFolder = new Map<string, string>();
  const picked = [...CATALOG.slice(0, 100), ...CATALOG.slice(100, 160)];
  for (const t of picked) {
    const suite = t.suitePath[1] ?? t.suitePath[0] ?? 'tests';
    let folderId = suiteFolder.get(suite);
    if (!folderId) {
      folderId = tmId(`folder:Automated/${suite}`);
      folders.push({ id: folderId, parentId: automatedRoot, name: suite, position: suiteFolder.size });
      suiteFolder.set(suite, folderId);
    }
    const res = byTestCase.get(t.testCaseId);
    const at = NOW - 2 * HOUR;
    addCase({
      name: t.name,
      status: 'active',
      origin: 'code',
      aiUnreviewed: false,
      folderId,
      body: body(`Automated test \`${t.fullName}\`.`, '', []),
      fields: { priority: 'Medium', severity: t.severity, owner: t.owner, layer: t.kind === 'ui' ? 'UI' : 'API', suite, epic: 'Web shop', feature: t.feature, story: t.story, component: [t.feature.replace(' API', '') === 'Payments' ? 'Payments' : t.feature] },
      tags: ['automated'],
      identities: [{ testCaseId: t.testCaseId, fullName: t.fullName, kind: 'code', lastSeenAt: iso(at), branches: ['main'] }],
      keys: [{ kind: 'allure_id', value: String(seq + 1) }],
      createdBy: 'ci',
      createdAt: iso(NOW - 90 * DAY),
      updatedBy: 'ci',
      updatedAt: iso(NOW - 3 * DAY),
      archivedAt: null,
      ...(res ? { lastResult: { status: res.status, at: res.startedAt ?? iso(at), source: 'automated' as const, launchId: res.launchId, resultId: res.id } } : {}),
    });
  }

  // TC-1 gets a history of versions whose steps move and change (stable ids).
  const tc1 = [...cases.values()].find((c) => c.name === 'Pay with a saved card');
  if (tc1) shapeTc1(tc1);

  // The AI draft.
  addCase({
    name: 'Expired coupon during checkout shows a clear error',
    status: 'draft',
    origin: 'ai',
    aiUnreviewed: true,
    folderId: names.get('Payment') ?? null,
    body: body(
      'Drafted from the failure of `applies a discount code` in launch #41: the coupon expired between adding it and paying.',
      '- A coupon `SPRING10` that expires in 2 minutes',
      [
        step('ai:1', 'Add *Trail running shoes* to the cart and open checkout', 'The order summary is shown', ''),
        step('ai:2', 'Apply the coupon `SPRING10`', 'The discount is applied to the total', 'coupon: `SPRING10`'),
        step('ai:3', 'Wait until the coupon expires, then press **Pay**', 'A message explains that the coupon expired and the total is recalculated', ''),
      ],
    ),
    fields: { priority: 'High', severity: 'normal', component: ['Checkout'], layer: 'UI' },
    tags: ['ai-draft'],
    identities: [],
    keys: [],
    createdBy: `agent:${AI_AGENT}`,
    createdAt: iso(NOW - 3 * HOUR),
    updatedBy: `agent:${AI_AGENT}`,
    updatedAt: iso(NOW - 3 * HOUR),
    archivedAt: null,
  });

  const tc2 = [...cases.values()].find((c) => c.name === 'Apply a discount code at checkout');
  const suggestions: CaseSuggestion[] = [];
  if (tc2) {
    const steps = structuredClone(tc2.body.steps);
    if (steps[1]) steps[1] = { ...steps[1], expected: 'The discount is applied and the saved amount is shown next to the total' };
    steps.push(step('sugg:new', 'Remove the coupon', 'The total returns to the full price', ''));
    suggestions.push({
      id: tmId('suggestion:1'),
      caseNumber: tc2.number,
      baseRevision: tc2.revision,
      proposal: { body: { ...tc2.body, steps }, tags: [...new Set([...tc2.tags, 'coupons'])] },
      rationale: 'The last three failures of `applies a discount code` were about removing a coupon, which the case does not cover.',
      agent: AI_AGENT,
      status: 'pending',
      createdAt: iso(NOW - 5 * HOUR),
      decidedBy: null,
      decidedAt: null,
    });
  }

  // Tests without a case (feature branches, new tests).
  const identities: TestIdentity[] = CATALOG.slice(160, 178).map((t, i) => ({
    testCaseId: t.testCaseId,
    fullName: t.fullName,
    displayName: t.name,
    kind: 'code',
    caseNumber: null,
    firstSeenAt: iso(NOW - (i + 2) * DAY),
    lastSeenAt: iso(NOW - i * HOUR),
    branches: i % 3 === 0 ? ['feature/coupon-v2'] : i % 3 === 1 ? ['feature/search-ranking', 'main'] : ['main'],
  }));

  const state: TmState = {
    seq,
    folders,
    fields: initialFields(),
    cases,
    suggestions,
    identities,
    plans: [],
    runItems: [],
    manual: new Map(),
    createdLaunches: new Map(),
    executions: [],
    attachments: new Map(),
    jobs: new Map(),
  };
  buildPlans(state, names);
  buildRun(state);
  return state;
}

function shapeTc1(c: MockCase): void {
  const s1 = step('tc1:1', 'Open the cart with *Trail running shoes* (size 42)', 'The cart lists one item', '');
  const s2 = step('tc1:2', 'Press **Checkout** and keep the saved address', 'The payment step opens', '');
  const s3 = step('tc1:3', 'Choose the saved card ending in 4242', 'The card is selected', 'card: `4242`');
  const s4 = step('tc1:4', 'Press **Pay**', 'The confirmation page shows the order number', '');
  const s5 = step('tc1:5', 'Open **Account → Orders**', 'The new order is listed as *Paid*', '');
  const pre = '- A customer with a saved card\n- Product `TRS-42` in stock';
  const snap = (steps: CaseStep[], extra: Partial<CaseSnapshot> = {}): CaseSnapshot => ({ ...snapshotOf(c), body: { ...c.body, preconditions: pre, steps: structuredClone(steps) }, ...extra });
  const t0 = NOW - 40 * DAY;
  const v1 = snap([s1, s2, s4], { status: 'draft', tags: [] });
  const v2 = snap([s1, s2, s3, s4]);
  const s4b = { ...s4, expected: 'The confirmation page shows the order number and the masked card' };
  const v3 = snap([s1, s3, s2, s4b], { fields: { ...c.fields, priority: 'Critical' } });
  const s3b = { ...s3, data: 'card: `4242` (expires 12/30)' };
  const v4 = snap([s1, s2, s3b, s4b, s5], { fields: { ...c.fields, priority: 'Critical' }, tags: ['regression', 'smoke'] });
  c.versions = [
    { version: 1, source: 'ui', changed: ['created'], createdBy: 'maria@example.com', createdAt: iso(t0), snapshot: v1 },
    { version: 2, source: 'ui', changed: ['status', 'tags', 'body.steps'], createdBy: 'maria@example.com', createdAt: iso(t0 + 2 * DAY), snapshot: v2 },
    { version: 3, source: 'ui', changed: ['fields.priority', 'body.steps'], createdBy: 'oleh@example.com', createdAt: iso(t0 + 9 * DAY), snapshot: v3 },
    { version: 4, source: 'import', changed: ['tags', 'body.steps'], createdBy: ME, createdAt: iso(t0 + 20 * DAY), snapshot: v4 },
  ];
  c.name = v4.name;
  c.status = v4.status;
  c.body = structuredClone(v4.body);
  c.fields = structuredClone(v4.fields);
  c.tags = [...v4.tags];
  c.version = 4;
  c.revision = 4;
  c.updatedAt = iso(t0 + 20 * DAY);
  c.updatedBy = ME;
  c.keys = [{ kind: 'testrail', value: 'C1001' }];
}

export function subtreeIds(state: Pick<TmState, 'folders'>, id: string): Set<string> {
  const out = new Set([id]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const f of state.folders) {
      if (f.parentId && out.has(f.parentId) && !out.has(f.id)) {
        out.add(f.id);
        grew = true;
      }
    }
  }
  return out;
}

function buildPlans(state: TmState, names: Map<string, string>): void {
  const all = [...state.cases.values()];
  const inFolders = (list: string[]) => {
    const ids = new Set<string>();
    for (const n of list) {
      const f = names.get(n);
      if (f) for (const x of subtreeIds(state, f)) ids.add(x);
    }
    return all.filter((c) => c.folderId && ids.has(c.folderId));
  };
  const manualRegression = inFolders(['Payment', 'Cart', 'Sign-in']).filter((c) => c.identities.length === 0 && c.origin !== 'ai').slice(0, 40);
  const automated = all.filter((c) => c.identities.length > 0).slice(0, 6);
  const assigneeFor = (i: number): string | null => (i < 4 ? ME : i < 16 ? 'maria@example.com' : i < 28 ? 'oleh@example.com' : null);
  const regressionItems: MockPlanItem[] = [...manualRegression, ...automated].map((c, i) => ({ caseNumber: c.number, assignee: c.identities.length ? null : assigneeFor(i), viaFilter: false, position: i }));
  const created = iso(NOW - 6 * DAY);
  state.plans.push({
    id: tmId('plan:regression'),
    kind: 'managed',
    name: 'Release 2.14 regression',
    description: 'Manual regression of payments, cart and sign-in before the 2.14 release, with the checkout autotests.',
    filter: null,
    counts: { total: 0, manual: 0, automated: 0 },
    revision: 3,
    createdBy: 'maria@example.com',
    createdAt: created,
    updatedAt: iso(NOW - 2 * DAY),
    archived: false,
    items: regressionItems,
  });

  const checkout = names.get('Checkout');
  const smokeMatches = inFolders(['Checkout']).filter((c) => c.status === 'active' && c.identities.length === 0);
  const stale = all.find((c) => c.folderId === names.get('Cart'));
  const items: MockPlanItem[] = smokeMatches.slice(2).map((c, i) => ({ caseNumber: c.number, assignee: i % 2 ? 'oleh@example.com' : ME, viaFilter: true, position: i }));
  if (stale) items.push({ caseNumber: stale.number, assignee: null, viaFilter: true, position: items.length });
  state.plans.push({
    id: tmId('plan:checkout-smoke'),
    kind: 'managed',
    name: 'Checkout smoke',
    description: 'Every active manual checkout case. Kept in sync with its filter.',
    filter: checkout ? { folderIds: [checkout], recursive: true, statuses: ['active'], automation: 'manual' } : null,
    counts: { total: 0, manual: 0, automated: 0 },
    revision: 1,
    createdBy: ME,
    createdAt: iso(NOW - 20 * DAY),
    updatedAt: iso(NOW - 20 * DAY),
    archived: false,
    items,
  });

  const mobile = inFolders(['Mobile web']).slice(0, 3);
  state.plans.push({
    id: tmId('plan:mobile'),
    kind: 'managed',
    name: 'Mobile web sanity',
    description: 'Three quick checks on a phone.',
    filter: null,
    counts: { total: 0, manual: 0, automated: 0 },
    revision: 1,
    createdBy: 'sam@example.com',
    createdAt: iso(NOW - 3 * DAY),
    updatedAt: null,
    archived: false,
    items: mobile.map((c, i) => ({ caseNumber: c.number, assignee: ME, viaFilter: false, position: i })),
  });
}

export function folderPathNames(state: Pick<TmState, 'folders'>, folderId: string | null): string[] {
  const out: string[] = [];
  let id = folderId;
  for (let i = 0; id && i < 20; i++) {
    const f = state.folders.find((x) => x.id === id);
    if (!f) break;
    out.unshift(f.name);
    id = f.parentId;
  }
  return out;
}

export function runItemFor(state: TmState, launchId: string, c: MockCase, assignee: string | null, i: number): MockRunItem {
  return {
    id: tmId(`run:${launchId}:${String(c.number)}:${String(i)}`),
    launchId,
    caseNumber: c.number,
    caseVersion: c.version,
    name: c.name,
    folderPath: folderPathNames(state, c.folderId),
    status: 'untested',
    assignee,
    executionNo: 1,
    updatedAt: iso(NOW - DAY),
    preconditions: c.body.preconditions,
    steps: expandSharedForRun(structuredClone(c.body.steps)),
    stepResults: [],
    comment: '',
    defect: null,
    executedBy: null,
    startedAt: null,
    finishedAt: null,
    revision: 1,
    resultId: null,
  };
}

function leafPaths(steps: CaseStep[], prefix = ''): string[] {
  const out: string[] = [];
  steps.forEach((s, i) => {
    const p = prefix ? `${prefix}.${String(i + 1)}` : String(i + 1);
    if (s.steps.length) out.push(...leafPaths(s.steps, p));
    else out.push(p);
  });
  return out;
}

function fill(item: MockRunItem, status: RunStatus, by: string, at: number): void {
  const leaves = leafPaths(item.steps);
  const res = (path: string, s: StepStatus, actual = ''): RunStepResult => ({ path, status: s, actual, comment: '', attachments: [], updatedBy: by, updatedAt: iso(at) });
  if (status === 'passed') item.stepResults = leaves.map((p) => res(p, 'passed'));
  else if (status === 'failed') item.stepResults = leaves.map((p, i) => (i === leaves.length - 1 ? res(p, 'failed', 'The confirmation page stays blank for 30 s, then shows **Something went wrong**.') : res(p, 'passed')));
  else if (status === 'blocked') item.stepResults = leaves.slice(0, 2).map((p, i) => (i === 1 ? res(p, 'blocked', 'The staging payment gateway is down.') : res(p, 'passed')));
  else if (status === 'skipped') item.stepResults = leaves.map((p) => res(p, 'skipped'));
  else if (status === 'in_progress') item.stepResults = leaves.slice(0, 1).map((p) => res(p, 'passed'));
  item.status = status;
  item.executedBy = status === 'untested' ? null : by;
  item.startedAt = status === 'untested' ? null : iso(at - 6 * MIN);
  item.finishedAt = status === 'untested' || status === 'in_progress' ? null : iso(at);
  item.updatedAt = iso(at);
  item.revision = 1 + item.stepResults.length;
  item.resultId = status === 'untested' || status === 'in_progress' ? null : tmId(`result:${item.id}:1`);
}

function buildRun(state: TmState): void {
  const launchId = manualLaunchId();
  const plan = state.plans[0];
  if (!launchId || !plan) return;
  state.manual.set(launchId, { launchId, planId: plan.id, state: 'open' });
  const plan2 = ['passed', 'passed', 'failed', 'passed', 'passed', 'blocked', 'passed', 'passed', 'failed', 'passed', 'in_progress', 'passed'] as RunStatus[];
  const oleh = ['passed', 'passed', 'skipped', 'passed', 'passed', 'passed', 'untested', 'untested', 'untested', 'untested', 'untested', 'untested'] as RunStatus[];
  let i = 0;
  for (const it of plan.items) {
    const c = state.cases.get(it.caseNumber);
    if (!c || c.identities.length) continue;
    const item = runItemFor(state, launchId, c, it.assignee, 0);
    const at = NOW - 20 * HOUR + i * 9 * MIN;
    if (it.assignee === ME) {
      if (i === 0) fill(item, 'in_progress', ME, at);
    } else if (it.assignee === 'maria@example.com') fill(item, plan2[(i - 4) % plan2.length] ?? 'passed', 'maria@example.com', at);
    else if (it.assignee === 'oleh@example.com') fill(item, oleh[(i - 16) % oleh.length] ?? 'untested', 'oleh@example.com', at);
    state.runItems.push(item);
    i++;
  }
}

/** A preview shown when the uploaded file cannot be read as CSV (for example a Qase JSON export). */
export const FALLBACK_PREVIEW: ImportPreview = {
  format: 'qase-json',
  headers: ['title', 'suite', 'description', 'preconditions', 'steps', 'priority', 'tags'],
  rows: [
    ['Refund a paid order', 'Payments/Refunds', 'Refund the whole amount', 'A paid order', '1. Open the order | Refund is offered', 'high', 'regression'],
    ['Partial refund', 'Payments/Refunds', 'Refund one item', 'A paid order with two items', '1. Refund one item | The rest stays paid', 'medium', ''],
  ],
  suggestedMapping: { title: 'name', suite: 'folder', description: 'description', preconditions: 'preconditions', steps: 'steps', priority: 'fields.priority', tags: 'tags' },
  stepsLayout: 'cells',
};
