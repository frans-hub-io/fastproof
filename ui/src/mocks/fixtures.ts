import type {
  Attachment,
  AttachmentKind,
  Attempt,
  AuditEvent,
  Counts,
  GrantableRole,
  Label,
  Launch,
  Project,
  ResultDetail,
  ResultStatus,
  ResultSummary,
  Step,
  StatusError,
  TestCaseHistoryItem,
  TokenInfo,
  TokenOptions,
  TreeGroupBy,
  TreeNode,
  TreeTest,
  Upload,
} from '../api/types';
import { actualPng, diffPng, expectedPng, screenshotPng } from './images';
import { sampleWebm } from './video';

export const NOW = Date.now();
const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

function iso(t: number): string {
  return new Date(t).toISOString();
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

function hash32(text: string, seed = 0x811c9dc5): number {
  let h = seed >>> 0;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h >>> 0;
}

function hex(text: string, len = 32): string {
  let out = '';
  let i = 0;
  while (out.length < len) {
    out += hash32(text, 0x811c9dc5 + i * 7919).toString(16).padStart(8, '0');
    i++;
  }
  return out.slice(0, len);
}

function uuid(text: string): string {
  const h = hex(text, 32);
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-7${h.slice(13, 16)}-8${h.slice(17, 20)}-${h.slice(20, 32)}`;
}

export function emptyCounts(): Counts {
  return { total: 0, passed: 0, failed: 0, broken: 0, skipped: 0, unknown: 0, flaky: 0, muted: 0 };
}

type FailureKind = 'timeout' | 'status' | 'cart' | 'visual' | 'db' | 'fixture' | 'conflict' | 'null';

interface TestDef {
  index: number;
  name: string;
  fullName: string;
  suitePath: string[];
  feature: string;
  story: string;
  pkg: string;
  kind: 'ui' | 'api';
  severity: string;
  owner: string;
  fragility: number;
  failure: FailureKind;
  alwaysSkipped: boolean;
  unknownStatus: boolean;
  parameters: { name: string; value: string }[];
  testCaseId: string;
  baseDuration: number;
}

const UI_SUITES: { feature: string; file: string; tests: string[] }[] = [
  {
    feature: 'Checkout',
    file: 'checkout.spec.ts',
    tests: [
      'pays with saved card', 'pays with a new card', 'applies a discount code', 'rejects an expired card', 'shows the order summary',
      'keeps the cart after a failed payment', 'charges shipping for heavy items', 'offers express delivery', 'splits the order by warehouse',
      'sends the confirmation email', 'pays with a gift card', 'combines gift card and card', 'validates the billing address',
      'remembers the delivery address', 'shows taxes per region', 'handles 3-D Secure challenge', 'cancels a pending payment',
      'retries a declined card', 'shows the receipt', 'downloads the invoice PDF',
    ],
  },
  {
    feature: 'Cart',
    file: 'cart.spec.ts',
    tests: [
      'adds a product to the cart', 'removes a product', 'updates the quantity', 'keeps the cart between sessions', 'merges guest cart on login',
      'limits quantity to stock', 'shows recommended items', 'applies bundle prices', 'empties the cart', 'shows the cart badge count',
      'saves an item for later', 'moves a saved item back', 'warns about price changes', 'shows delivery estimate', 'recalculates totals',
      'handles out-of-stock items', 'shows the free shipping threshold', 'restores a removed item', 'keeps notes per item', 'shares the cart link',
    ],
  },
  {
    feature: 'Catalog',
    file: 'catalog.spec.ts',
    tests: [
      'lists products of a category', 'filters by price', 'filters by brand', 'sorts by rating', 'paginates results', 'shows product details',
      'switches product variants', 'zooms product images', 'shows reviews', 'writes a review', 'shows availability per store', 'compares two products',
      'shows breadcrumbs', 'shows related products', 'opens the size guide', 'keeps filters in the URL', 'shows the sale badge',
      'hides discontinued products', 'shows the price history', 'adds to the wish list',
    ],
  },
  {
    feature: 'Search',
    file: 'search.spec.ts',
    tests: [
      'finds products by name', 'suggests while typing', 'corrects typos', 'finds by SKU', 'shows no-results help', 'searches within a category',
      'highlights matched words', 'keeps recent searches', 'clears recent searches', 'searches in another language', 'ranks exact matches first',
      'finds by barcode', 'filters search results', 'opens a result in a new tab', 'handles very long queries', 'escapes special characters',
      'shows popular searches', 'searches by voice', 'paginates search results', 'shows sponsored results',
    ],
  },
  {
    feature: 'Account',
    file: 'account.spec.ts',
    tests: [
      'signs up with email', 'signs in with password', 'signs out', 'resets the password', 'changes the email', 'enables two-factor sign-in',
      'lists past orders', 'reorders a past order', 'manages saved cards', 'manages addresses', 'downloads personal data', 'deletes the account',
      'updates newsletter preferences', 'shows loyalty points', 'redeems loyalty points', 'locks after failed sign-ins', 'signs in with SSO',
      'keeps the session after reload', 'expires an idle session', 'shows the profile page',
    ],
  },
];

const API_SUITES: { feature: string; pkg: string; cls: string; tests: string[] }[] = [
  {
    feature: 'Orders API',
    pkg: 'com.example.shop.orders',
    cls: 'OrdersApiTest',
    tests: [
      'createsOrder', 'rejectsEmptyOrder', 'listsOrdersOfCustomer', 'paginatesOrders', 'cancelsOrder', 'refusesCancelAfterShipping', 'updatesShippingAddress',
      'calculatesTotals', 'appliesCoupon', 'rejectsUnknownCoupon', 'returnsOrderById', 'returns404ForUnknownOrder', 'exportsOrdersCsv', 'archivesOldOrders',
      'emitsOrderCreatedEvent', 'keepsIdempotencyKey', 'rejectsDuplicateKey', 'locksStockOnCreate', 'releasesStockOnCancel', 'auditsStatusChanges',
    ],
  },
  {
    feature: 'Payments API',
    pkg: 'com.example.shop.payments',
    cls: 'PaymentsApiTest',
    tests: [
      'authorizesCard', 'capturesPayment', 'refundsPayment', 'partiallyRefunds', 'rejectsExpiredCard', 'handlesDeclinedCard', 'requires3dsChallenge',
      'completes3dsChallenge', 'voidsAuthorization', 'reconcilesSettlements', 'signsWebhook', 'rejectsBadWebhookSignature', 'retriesWebhook',
      'masksCardNumberInLogs', 'convertsCurrency', 'roundsAmounts', 'limitsRefundToCaptured', 'listsPaymentsOfOrder', 'exportsSettlementReport', 'timesOutGateway',
    ],
  },
  {
    feature: 'Inventory API',
    pkg: 'com.example.shop.inventory',
    cls: 'InventoryApiTest',
    tests: [
      'reservesStock', 'releasesReservation', 'expiresReservation', 'syncsWarehouse', 'reportsLowStock', 'importsStockFile', 'rejectsNegativeStock',
      'movesStockBetweenWarehouses', 'countsAvailableStock', 'locksConcurrently', 'listsWarehouses', 'returnsSkuAvailability', 'auditsAdjustments',
      'rebuildsSearchIndex', 'handlesBackorders', 'notifiesRestock', 'keepsHistory', 'mergesDuplicateSkus', 'exportsInventory', 'validatesSkuFormat',
    ],
  },
  {
    feature: 'Admin',
    pkg: 'com.example.shop.admin',
    cls: 'AdminApiTest',
    tests: [
      'createsProduct', 'updatesPrice', 'schedulesSale', 'publishesCategory', 'importsCatalog', 'rejectsInvalidImport', 'managesStaffRoles',
      'auditsAdminActions', 'exportsCustomers', 'anonymizesCustomer', 'createsPromotion', 'expiresPromotion', 'listsReturns', 'approvesReturn',
      'rejectsReturn', 'printsShippingLabel', 'syncsMarketplace', 'reindexesProducts', 'purgesCache', 'rotatesApiKeys',
    ],
  },
  {
    feature: 'Search API',
    pkg: 'com.example.shop.search',
    cls: 'SearchApiTest',
    tests: [
      'indexesProduct', 'deletesFromIndex', 'searchesByPrefix', 'searchesFuzzy', 'boostsInStock', 'facetsByBrand', 'facetsByPrice', 'sortsByPrice',
      'paginatesWithCursor', 'limitsPageSize', 'returnsSuggestions', 'logsQueries', 'ranksByPopularity', 'filtersByCategory', 'handlesUnicode',
      'rejectsTooLongQuery', 'cachesHotQueries', 'expiresCache', 'returnsSynonyms', 'reportsZeroResults',
    ],
  },
];

const OWNERS = ['team-checkout', 'team-catalog', 'team-platform', 'team-payments'];
const SEVERITIES = ['blocker', 'critical', 'normal', 'normal', 'normal', 'minor', 'trivial'];
const FAILURES: FailureKind[] = ['timeout', 'status', 'cart', 'visual', 'timeout', 'conflict', 'null'];

function buildCatalog(): TestDef[] {
  const defs: TestDef[] = [];
  const r = rng(7);
  let index = 0;
  const push = (d: Omit<TestDef, 'index' | 'fragility' | 'failure' | 'alwaysSkipped' | 'unknownStatus' | 'testCaseId' | 'baseDuration' | 'severity' | 'owner'>) => {
    const roll = r();
    const fragile = roll < 0.16;
    const def: TestDef = {
      ...d,
      index,
      severity: SEVERITIES[Math.floor(r() * SEVERITIES.length)] ?? 'normal',
      owner: OWNERS[Math.floor(r() * OWNERS.length)] ?? 'team-platform',
      fragility: fragile ? 0.35 + r() * 0.5 : r() < 0.3 ? 0.03 : 0,
      failure: d.kind === 'api' ? (r() < 0.25 ? 'db' : r() < 0.5 ? 'fixture' : r() < 0.75 ? 'conflict' : 'status') : (FAILURES[Math.floor(r() * FAILURES.length)] ?? 'timeout'),
      alwaysSkipped: !fragile && r() < 0.06,
      unknownStatus: false,
      testCaseId: hex(`tc:${d.fullName}`),
      baseDuration: d.kind === 'ui' ? 1500 + r() * 9000 : 40 + r() * 900,
    };
    defs.push(def);
    index++;
  };
  for (const s of UI_SUITES) {
    for (const t of s.tests) {
      const isSearch = s.feature === 'Search' && t === 'finds products by name';
      push({
        name: t,
        fullName: `e2e/${s.file} > ${s.feature} > ${t}`,
        suitePath: ['e2e', s.file, s.feature],
        feature: s.feature,
        story: t.split(' ').slice(0, 2).join(' '),
        pkg: `e2e.${s.file.replace('.spec.ts', '')}`,
        kind: 'ui',
        parameters: isSearch ? [{ name: 'term', value: 'running shoes' }] : [{ name: 'browser', value: 'chromium' }],
      });
    }
  }
  for (const s of API_SUITES) {
    for (const t of s.tests) {
      push({
        name: t,
        fullName: `${s.pkg}.${s.cls}.${t}`,
        suitePath: [s.pkg, s.cls],
        feature: s.feature,
        story: t.replace(/([A-Z])/g, ' $1').toLowerCase().trim().split(' ').slice(0, 2).join(' '),
        pkg: s.pkg,
        kind: 'api',
        parameters: [],
      });
    }
  }
  const rich = defs[0];
  if (rich) {
    rich.fragility = 0.6;
    rich.failure = 'visual';
    rich.alwaysSkipped = false;
    rich.severity = 'blocker';
    rich.owner = 'team-checkout';
  }
  const cart = defs[22];
  if (cart) {
    cart.fragility = 0.7;
    cart.failure = 'cart';
    cart.alwaysSkipped = false;
  }
  const unknown = defs[150];
  if (unknown) unknown.unknownStatus = true;
  return defs;
}

export const CATALOG = buildCatalog();

interface EnvDef {
  key: string;
  values: Record<string, string>;
}

const ENVS: EnvDef[] = [
  { key: 'staging-chromium', values: { stage: 'staging', browser: 'chromium', os: 'linux' } },
  { key: 'staging-firefox', values: { stage: 'staging', browser: 'firefox', os: 'linux' } },
  { key: 'production-smoke', values: { stage: 'production', browser: 'chromium', os: 'linux' } },
];

const DEFAULT_ENV: EnvDef = { key: 'staging-chromium', values: { stage: 'staging', browser: 'chromium', os: 'linux' } };

const BRANCHES = ['main', 'main', 'main', 'feature/checkout-v2', 'release/2026.11', 'fix/cart-badge'];

interface LaunchDef {
  project: string;
  launch: Launch;
  tests: TestDef[];
  seed: number;
  progress?: () => number;
}

const RUNNING_START = NOW - 4 * MIN;

function counts(results: readonly ResultSummary[]): Counts {
  const c = emptyCounts();
  for (const r of results) {
    c.total++;
    c[r.status]++;
    if (r.flakyInRun) c.flaky++;
    if (r.muted) c.muted++;
  }
  return c;
}

function commit(seed: string): string {
  return hex(`commit:${seed}`, 40);
}

function makeLaunch(project: string, number: number, opts: { startedAt: number; status: Launch['status']; branch: string; env: EnvDef; debug?: boolean; name?: string }): Launch {
  const id = uuid(`launch:${project}:${String(number)}`);
  const pipeline = String(81000 + number * 7);
  const repo = project === 'payments-api' ? 'https://github.com/example/payments-api' : `https://gitlab.example.com/shop/${project}`;
  const provider = project === 'payments-api' ? 'github' : 'gitlab';
  const ciUrl = provider === 'github' ? `${repo}/actions/runs/${pipeline}` : `${repo}/-/pipelines/${pipeline}`;
  return {
    id,
    number,
    name: opts.name ?? (opts.branch === 'main' ? `${project} nightly` : `${project} ${opts.branch}`),
    status: opts.status,
    debug: opts.debug ?? false,
    startedAt: iso(opts.startedAt),
    finishedAt: opts.status === 'running' ? null : iso(opts.startedAt + 14 * MIN + (number % 7) * MIN),
    durationMs: opts.status === 'running' ? null : 14 * MIN + (number % 7) * MIN,
    ci: { provider, pipelineId: pipeline, jobId: String(556000 + number * 13), url: ciUrl, buildOrder: number },
    vcs: { repository: repo, branch: opts.branch, commit: commit(`${project}:${String(number)}`) },
    environment: { key: opts.env.key, values: { ...opts.env.values } },
    attributes: [
      { key: 'suite', value: project === 'web-shop' ? 'regression' : 'contract' },
      { key: 'trigger', value: number % 3 === 0 ? 'merge_request' : 'schedule' },
    ],
    counts: emptyCounts(),
    uploads: { queued: 0, processing: 0, failed: 0 },
    important: false,
  };
}

const launches = new Map<string, LaunchDef>();
const launchOrder = new Map<string, string[]>();

function addLaunch(def: LaunchDef): void {
  launches.set(def.launch.id, def);
  const list = launchOrder.get(def.project) ?? [];
  list.push(def.launch.id);
  launchOrder.set(def.project, list);
}

function buildLaunches(): void {
  for (let n = 42; n >= 1; n--) {
    const age = n === 42 ? 0 : n === 41 ? HOUR : (42 - n) * 0.5 * DAY + (n % 3) * HOUR;
    const status: Launch['status'] = n === 42 ? 'running' : n === 39 || n === 23 ? 'interrupted' : 'finished';
    const branch = n === 41 || n === 42 || n === 39 ? 'main' : n === 40 ? 'feature/checkout-v2' : (BRANCHES[n % BRANCHES.length] ?? 'main');
    const env = (n === 40 ? ENVS[1] : n === 38 || n % 5 === 0 ? ENVS[2] : ENVS[0]) ?? DEFAULT_ENV;
    const launch = makeLaunch('web-shop', n, {
      startedAt: n === 42 ? RUNNING_START : NOW - age - 15 * MIN,
      status,
      branch,
      env,
      debug: n === 17,
    });
    const tests = env === ENVS[2] ? CATALOG.filter((t) => t.kind === 'ui' && t.index % 4 === 0) : n === 39 ? CATALOG.slice(0, 130) : CATALOG.slice(0, n === 40 ? 198 : 200);
    addLaunch({ project: 'web-shop', launch, tests, seed: n, progress: n === 42 ? () => Math.min(0.95, 0.35 + (Date.now() - NOW) / 240_000) : undefined });
  }
  for (let n = 3; n >= 1; n--) {
    const launch = makeLaunch('payments-api', n, { startedAt: NOW - (4 - n) * DAY, status: 'finished', branch: 'main', env: { key: 'ci', values: { jdk: '21' } }, name: 'payments-api contract' });
    addLaunch({ project: 'payments-api', launch, tests: CATALOG.filter((t) => t.feature === 'Payments API' || t.feature === 'Orders API'), seed: 100 + n });
  }
}

buildLaunches();

const RICH_LAUNCH = uuid('launch:web-shop:41');
export const RICH_LAUNCH_ID = RICH_LAUNCH;
export const RUNNING_LAUNCH_ID = uuid('launch:web-shop:42');

function statusFor(t: TestDef, seed: number): { status: ResultStatus; flaky: boolean; attempts: number } {
  if (t.alwaysSkipped) return { status: 'skipped', flaky: false, attempts: 1 };
  if (t.unknownStatus && seed % 2 === 1) return { status: 'unknown', flaky: false, attempts: 1 };
  const r = rng(seed * 7919 + t.index * 104729);
  const x = r();
  if (t.index === 0) {
    if (seed === 41 || seed === 39 || seed === 42) return { status: 'failed', flaky: false, attempts: 2 };
    if (seed === 40) return { status: 'passed', flaky: true, attempts: 2 };
  }
  if (x < t.fragility * 0.5) {
    const broken = t.failure === 'db' || t.failure === 'fixture' || t.failure === 'null';
    return { status: broken ? 'broken' : 'failed', flaky: false, attempts: t.kind === 'ui' ? 2 : 1 };
  }
  if (x < t.fragility * 0.75) return { status: 'passed', flaky: true, attempts: 2 };
  if (r() < 0.04) return { status: 'skipped', flaky: false, attempts: 1 };
  return { status: 'passed', flaky: false, attempts: 1 };
}

const ERRORS: Record<FailureKind, (seed: number) => StatusError> = {
  timeout: (seed) => ({
    message: `TimeoutError: locator.click: Timeout ${String(30000 + (seed % 3) * 5000)}ms exceeded.\nCall log:\n  - waiting for getByRole('button', { name: 'Continue' })`,
    trace: `TimeoutError: locator.click: Timeout 30000ms exceeded.\n    at CheckoutPage.continue (e2e/pages/checkout.ts:48:22)\n    at e2e/checkout.spec.ts:31:5`,
    actual: null,
    expected: null,
  }),
  status: (seed) => ({
    message: `Error: expect(received).toBe(expected) // Object.is equality\n\nExpected: 200\nReceived: ${String(500 + (seed % 4))}`,
    trace: `Error: expect(received).toBe(expected)\n    at e2e/helpers/api.ts:22:31\n    at e2e/cart.spec.ts:57:9`,
    actual: String(500 + (seed % 4)),
    expected: '200',
  }),
  cart: () => ({
    message: 'AssertionError: expected cart to contain 3 items but found 2',
    trace: 'AssertionError: expected cart to contain 3 items but found 2\n    at CartPage.expectItems (e2e/pages/cart.ts:91:11)\n    at e2e/cart.spec.ts:102:3',
    expected: JSON.stringify(
      {
        items: [
          { sku: 'SHOE-42-BLK', qty: 1, price: 89.9 },
          { sku: 'SOCK-M-WHT', qty: 2, price: 7.5 },
          { sku: 'CAP-ONE-RED', qty: 1, price: 19.0 },
        ],
        total: 123.9,
        currency: 'EUR',
      },
      null,
      2,
    ),
    actual: JSON.stringify(
      {
        items: [
          { sku: 'SHOE-42-BLK', qty: 1, price: 89.9 },
          { sku: 'SOCK-M-WHT', qty: 1, price: 7.5 },
        ],
        total: 97.4,
        currency: 'EUR',
      },
      null,
      2,
    ),
  }),
  visual: (seed) => ({
    message: `Error: Screenshot comparison failed:\n\n  ${String(1200 + (seed % 9) * 17)} pixels (ratio 0.02 of all image pixels) are different.\n\nExpected: checkout-summary-expected.png\nReceived: checkout-summary-actual.png`,
    trace: `Error: Screenshot comparison failed\n    at e2e/checkout.spec.ts:18:30\n    at runTest (node_modules/@playwright/test/lib/worker/workerMain.js:342:7)`,
    expected: 'Pay now button: purple, 124×32 at (316, 214)',
    actual: 'Pay now button: red, 140×36 at (300, 206)',
  }),
  db: (seed) => ({
    message: `java.sql.SQLTransientConnectionException: HikariPool-1 - Connection is not available, request timed out after ${String(30000 + seed)}ms.`,
    trace:
      'java.sql.SQLTransientConnectionException: HikariPool-1 - Connection is not available\n\tat com.zaxxer.hikari.pool.HikariPool.createTimeoutException(HikariPool.java:696)\n\tat com.example.shop.db.Db.connection(Db.java:41)\n\tat com.example.shop.orders.OrdersApiTest.setUp(OrdersApiTest.java:37)',
    actual: null,
    expected: null,
  }),
  fixture: () => ({
    message: "java.lang.IllegalStateException: Test data fixture 'orders' is not loaded",
    trace: "java.lang.IllegalStateException: Test data fixture 'orders' is not loaded\n\tat com.example.shop.fixtures.Fixtures.require(Fixtures.java:58)\n\tat com.example.shop.payments.PaymentsApiTest.init(PaymentsApiTest.java:44)",
    actual: null,
    expected: null,
  }),
  conflict: () => ({
    message: 'org.opentest4j.AssertionFailedError: expected: <201> but was: <409>',
    trace: 'org.opentest4j.AssertionFailedError: expected: <201> but was: <409>\n\tat org.junit.jupiter.api.AssertionUtils.fail(AssertionUtils.java:55)\n\tat com.example.shop.orders.OrdersApiTest.createsOrder(OrdersApiTest.java:88)',
    actual: '409',
    expected: '201',
  }),
  null: () => ({
    message: "TypeError: Cannot read properties of null (reading 'price')",
    trace: "TypeError: Cannot read properties of null (reading 'price')\n    at formatPrice (src/lib/price.ts:12:18)\n    at e2e/catalog.spec.ts:66:14",
    actual: null,
    expected: null,
  }),
};

interface StoredAttachment {
  meta: Attachment;
  body: () => Uint8Array | string;
}

const attachmentStore = new Map<string, StoredAttachment>();

function b64(data: string): Uint8Array {
  const bin = atob(data);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

const ESC = '\u001b';

const ANSI_LOG = [
  `${ESC}[90m09:14:02.118${ESC}[0m ${ESC}[34mINFO${ESC}[0m  Starting browser ${ESC}[1mchromium${ESC}[0m 131.0 (headless)`,
  `${ESC}[90m09:14:02.502${ESC}[0m ${ESC}[34mINFO${ESC}[0m  GET https://shop.example.com/checkout ${ESC}[32m200${ESC}[0m 184 ms`,
  `${ESC}[90m09:14:03.011${ESC}[0m ${ESC}[33mWARN${ESC}[0m  Slow response from ${ESC}[4mhttps://api.example.com/cart${ESC}[24m: 2.4 s`,
  `${ESC}[90m09:14:04.730${ESC}[0m ${ESC}[34mINFO${ESC}[0m  POST https://api.example.com/payments ${ESC}[31m502${ESC}[0m 1210 ms`,
  `${ESC}[90m09:14:04.731${ESC}[0m ${ESC}[1;31mERROR${ESC}[0m Payment gateway answered ${ESC}[41;97m BAD GATEWAY ${ESC}[0m retrying (1/3)`,
  `${ESC}[90m09:14:05.902${ESC}[0m ${ESC}[34mINFO${ESC}[0m  POST https://api.example.com/payments ${ESC}[32m201${ESC}[0m 402 ms`,
  `${ESC}[90m09:14:06.420${ESC}[0m ${ESC}[38;5;208mDEBUG${ESC}[0m Comparing screenshot ${ESC}[3mcheckout-summary.png${ESC}[23m (threshold 0.2)`,
  `${ESC}[90m09:14:06.533${ESC}[0m ${ESC}[1;31mERROR${ESC}[0m ${ESC}[38;2;255;120;120m1342 pixels differ${ESC}[0m`,
].join('\n');

const HTTP_EXCHANGE = {
  schemaVersion: 1,
  request: {
    method: 'POST',
    url: 'https://api.example.com/payments?order=SO-2026-118842',
    headers: [
      { name: 'Content-Type', value: 'application/json' },
      { name: 'Authorization', value: '__ALLURE_REDACTED__' },
      { name: 'Idempotency-Key', value: '4f1c2a8e-63b0-4bd2-9d8e-0c5cfbd1a5e1' },
    ],
    cookies: [{ name: 'session', value: '__ALLURE_REDACTED__' }],
    body: { encoding: 'utf8', value: JSON.stringify({ amount: 123.9, currency: 'EUR', card: { token: 'tok_saved_4242' } }), size: 74 },
  },
  response: {
    status: 502,
    statusText: 'Bad Gateway',
    headers: [
      { name: 'Content-Type', value: 'application/json' },
      { name: 'Retry-After', value: '1' },
    ],
    body: { encoding: 'utf8', value: JSON.stringify({ code: 'gateway_unavailable', message: 'Upstream acquirer did not answer in time' }), size: 79 },
  },
  start: NOW - HOUR - 3 * MIN,
  stop: NOW - HOUR - 3 * MIN + 1210,
};

function attach(owner: string, name: string, contentType: string, kind: AttachmentKind, body: () => Uint8Array | string): Attachment {
  const id = uuid(`att:${owner}:${name}`);
  const sample = body();
  const size = typeof sample === 'string' ? new TextEncoder().encode(sample).length : sample.length;
  const meta: Attachment = { id, name, contentType, kind, size };
  attachmentStore.set(id, { meta, body });
  return meta;
}

export function attachmentContent(id: string): StoredAttachment | undefined {
  return attachmentStore.get(id);
}

function dataUri(b: string): string {
  return `data:image/png;base64,${b}`;
}

function richAttachments(owner: string): Attachment[] {
  return [
    attach(owner, 'screenshot.png', 'image/png', 'image', () => b64(screenshotPng)),
    attach(owner, 'checkout-summary-expected.png', 'image/png', 'image', () => b64(expectedPng)),
    attach(owner, 'checkout-summary-actual.png', 'image/png', 'image', () => b64(actualPng)),
    attach(owner, 'checkout-summary-diff.png', 'image/png', 'image', () => b64(diffPng)),
    attach(owner, 'Screenshot diff', 'application/vnd.allure.image.diff', 'imageDiff', () =>
      JSON.stringify({ expected: dataUri(expectedPng), actual: dataUri(actualPng), diff: dataUri(diffPng) }),
    ),
    attach(owner, 'video.webm', 'video/webm', 'video', () => b64(sampleWebm)),
    attach(owner, 'trace.zip', 'application/zip', 'trace', () => new Uint8Array([80, 75, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])),
    attach(owner, 'POST /payments', 'application/vnd.allure.http+json', 'httpExchange', () => JSON.stringify(HTTP_EXCHANGE)),
    attach(owner, 'order.json', 'application/json', 'json', () =>
      JSON.stringify({ id: 'SO-2026-118842', customer: { id: 'C-1042', email: 'buyer@example.com' }, items: [{ sku: 'SHOE-42-BLK', qty: 1 }, { sku: 'SOCK-M-WHT', qty: 2 }], total: 123.9, paid: false, tags: ['web', 'saved-card'], meta: null }),
    ),
    attach(owner, 'browser-console.log', 'text/x-log', 'log', () => ANSI_LOG),
    attach(owner, 'notes.txt', 'text/plain', 'text', () => 'Checkout summary layout changed after the design refresh.\nThe Pay now button moved 16 px to the left and turned red.\n'),
    attach(owner, 'report.html', 'text/html', 'other', () => '<!doctype html><html><body><h1>Embedded report</h1><script>alert(1)</script></body></html>'),
    attach(owner, 'badge.svg', 'image/svg+xml', 'image', () => '<svg xmlns="http://www.w3.org/2000/svg" width="80" height="20"><rect width="80" height="20" fill="#c00"/></svg>'),
    attach(owner, 'heap.bin', 'application/octet-stream', 'other', () => new Uint8Array([0, 1, 2, 3, 4, 5, 6, 7])),
  ];
}

function standardAttachments(owner: string, t: TestDef, failed: boolean): Attachment[] {
  if (t.kind === 'api') {
    return [
      attach(owner, `${t.name}.httpexchange`, 'application/vnd.allure.http+json', 'httpExchange', () => JSON.stringify({ ...HTTP_EXCHANGE, response: { ...HTTP_EXCHANGE.response, status: failed ? 409 : 201, statusText: failed ? 'Conflict' : 'Created' } })),
      attach(owner, 'test.log', 'text/plain', 'log', () => `${ESC}[34mINFO${ESC}[0m running ${t.fullName}\n${failed ? `${ESC}[31mFAIL${ESC}[0m` : `${ESC}[32mOK${ESC}[0m`}\n`),
    ];
  }
  if (!failed) return [];
  return [
    attach(owner, 'screenshot.png', 'image/png', 'image', () => b64(screenshotPng)),
    attach(owner, 'video.webm', 'video/webm', 'video', () => b64(sampleWebm)),
    attach(owner, 'trace.zip', 'application/zip', 'trace', () => new Uint8Array([80, 75, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])),
  ];
}

interface StoredResult {
  summary: ResultSummary;
  launchId: string;
  def: TestDef;
  attempts: () => Attempt[];
  startedAt: number;
}

const resultsByLaunch = new Map<string, StoredResult[]>();
const resultsById = new Map<string, StoredResult>();

function messageOf(t: TestDef, seed: number): StatusError {
  return ERRORS[t.failure](seed);
}

function labelsOf(t: TestDef, project: string): Label[] {
  const labels: Label[] = [
    { name: 'feature', value: t.feature },
    { name: 'story', value: t.story },
    { name: 'severity', value: t.severity },
    { name: 'owner', value: t.owner },
    { name: 'package', value: t.pkg },
    { name: 'framework', value: t.kind === 'ui' ? 'playwright' : 'junit5' },
    { name: 'host', value: `${project}-runner-${String((t.index % 4) + 1)}` },
  ];
  if (t.index % 5 === 0) labels.push({ name: 'tag', value: 'smoke' });
  if (t.alwaysSkipped) labels.push({ name: 'tag', value: 'quarantine' });
  return labels;
}

function stepsFor(t: TestDef, owner: string, start: number, duration: number, status: ResultStatus, error: StatusError | null, rich: boolean): Step[] {
  const failing = status === 'failed' || status === 'broken';
  let at = start;
  const mk = (name: string, part: number, extra: Partial<Step> = {}): Step => {
    const d = Math.max(1, Math.round(duration * part));
    const s: Step = {
      name,
      status: 'passed',
      kind: 'step',
      startedAt: iso(at),
      stoppedAt: iso(at + d),
      durationMs: d,
      error: null,
      parameters: [],
      attachments: [],
      steps: [],
      ...extra,
    };
    at += d;
    return s;
  };
  if (t.kind === 'api') {
    const steps = [
      mk('Load fixture data', 0.15, { kind: 'fixture' }),
      mk(`Send request ${t.name}`, 0.55, { parameters: [{ name: 'endpoint', value: `/api/${t.feature.split(' ')[0]?.toLowerCase() ?? 'x'}` }] }),
      mk('Check response', 0.3),
    ];
    const last = steps[steps.length - 1];
    if (failing && last) {
      last.status = status;
      last.error = error;
    }
    return steps;
  }
  const steps: Step[] = [
    mk('browser', 0.08, { kind: 'fixture' }),
    mk('page', 0.04, { kind: 'fixture' }),
    mk('Open the product page', 0.14, { parameters: [{ name: 'url', value: 'https://shop.example.com/p/running-shoe' }] }),
  ];
  const add = mk('Add the product to the cart', 0.2);
  at = new Date(add.startedAt ?? iso(at)).getTime();
  add.steps = [mk("Click 'Add to cart'", 0.08), mk('Wait for the cart badge', 0.12, { parameters: [{ name: 'expected', value: '1' }] })];
  at = new Date(add.stoppedAt ?? iso(at)).getTime();
  steps.push(add);
  steps.push(mk('Go to checkout', 0.16));
  const pay = mk('Pay for the order', 0.3, { parameters: [{ name: 'card', value: '•••• 4242' }] });
  at = new Date(pay.startedAt ?? iso(at)).getTime();
  pay.steps = [mk('Fill the payment form', 0.1), mk('Submit the order', 0.12), mk('Compare the summary screenshot', 0.08)];
  at = new Date(pay.stoppedAt ?? iso(at)).getTime();
  steps.push(pay);
  steps.push(mk('Close the browser', 0.04, { kind: 'fixture' }));
  if (failing) {
    pay.status = status;
    const last = pay.steps[pay.steps.length - 1];
    if (last) {
      last.status = status;
      last.error = error;
      last.attachments = rich
        ? [attach(owner, 'step-screenshot.png', 'image/png', 'image', () => b64(actualPng)), attach(owner, 'step-dom.json', 'application/json', 'json', () => JSON.stringify({ selector: "getByRole('button', { name: 'Pay now' })", visible: true, box: { x: 300, y: 206, w: 140, h: 36 } }))]
        : [attach(owner, 'failure.png', 'image/png', 'image', () => b64(screenshotPng))];
    }
  }
  const submit = pay.steps[1];
  if (submit && rich) {
    submit.attachments = [attach(owner, 'POST /payments (step)', 'application/vnd.allure.http+json', 'httpExchange', () => JSON.stringify(HTTP_EXCHANGE))];
  }
  return steps;
}

function buildResults(def: LaunchDef): StoredResult[] {
  const existing = resultsByLaunch.get(def.launch.id);
  if (existing) return existing;
  const out: StoredResult[] = [];
  const launchStart = new Date(def.launch.startedAt).getTime();
  let offset = 2000;
  for (const t of def.tests) {
    const { status, flaky, attempts: attemptCount } = statusFor(t, def.seed);
    const id = uuid(`result:${def.launch.id}:${t.testCaseId}`);
    const r = rng(def.seed * 31 + t.index);
    const richRun = def.launch.id === RICH_LAUNCH && t.index === 0;
    const duration = status === 'skipped' ? 0 : richRun ? 11_000 : Math.round(t.baseDuration * (0.7 + r() * 0.8));
    const start = launchStart + offset;
    offset += Math.round(duration / 4) + 300;
    const failing = status === 'failed' || status === 'broken';
    const error = failing ? messageOf(t, def.seed) : null;
    const rich = def.launch.id === RICH_LAUNCH && t.index === 0;
    const muted = t.index === 22 && failing;
    const summary: ResultSummary = {
      id,
      launchId: def.launch.id,
      testCaseId: t.testCaseId,
      seriesKey: `${t.testCaseId}.${hex(`params:${JSON.stringify(t.parameters)}`, 32)}`,
      name: t.name,
      fullName: t.fullName,
      suitePath: t.suitePath,
      status,
      flakyInRun: flaky,
      flaky,
      attemptCount,
      durationMs: duration,
      labels: labelsOf(t, def.project),
      muted,
      fingerprint: failing ? hex(`fp:${t.failure}${t.failure === 'null' && def.seed >= 40 ? ':v2' : ''}`, 16) : null,
      defect: muted
        ? {
            typeId: 'automationBug',
            group: 'automationBug',
            name: 'Automation bug',
            comment: 'Cart badge race, fix in progress',
            issueUrl: 'https://issues.example.com/browse/SHOP-1180',
            issueKey: 'SHOP-1180',
            source: 'user',
            at: iso(start),
            by: 'sam@example.com',
          }
        : null,
      muteUntil: muted ? iso(NOW + 9 * DAY) : null,
      message: error?.message ?? (flaky ? null : null),
      clientIds: Array.from({ length: attemptCount }, (_, i) => clientIdOf(id, i)),
    };
    const owner = id;
    const attempts = (): Attempt[] => {
      const list: Attempt[] = [];
      let at = start;
      for (let no = 1; no <= attemptCount; no++) {
        const isLast = no === attemptCount;
        const attemptStatus: ResultStatus = isLast ? status : flaky ? 'failed' : status;
        const attemptError = attemptStatus === 'failed' || attemptStatus === 'broken' ? (isLast ? error : messageOf({ ...t, failure: flaky ? 'timeout' : t.failure }, def.seed + no)) : null;
        const d = isLast ? duration : Math.round(duration * 1.2);
        const attemptOwner = `${owner}:${String(no)}`;
        const failingAttempt = attemptStatus === 'failed' || attemptStatus === 'broken';
        list.push({
          no,
          status: attemptStatus,
          startedAt: status === 'skipped' ? null : iso(at),
          stoppedAt: status === 'skipped' ? null : iso(at + d),
          durationMs: status === 'skipped' ? null : d,
          error: attemptError,
          steps: status === 'skipped' ? [] : stepsFor(t, attemptOwner, at, d, attemptStatus, attemptError, rich && isLast),
          attachments: rich && isLast ? richAttachments(attemptOwner) : standardAttachments(attemptOwner, t, failingAttempt),
          stdout:
            status === 'skipped'
              ? null
              : rich && isLast
                ? ANSI_LOG
                : `${ESC}[32m✓${ESC}[0m setup done\n${failingAttempt ? `${ESC}[31m✗ ${attemptError?.message?.split('\n')[0] ?? 'failed'}${ESC}[0m` : `${ESC}[32m✓${ESC}[0m ${t.name}`}\n`,
          stderr: failingAttempt ? `${ESC}[33mwarning${ESC}[0m: retrying is enabled (retries=1)\n` : null,
          traceId: t.kind === 'ui' ? hex(`trace:${attemptOwner}`, 32) : null,
        });
        at += d + 500;
      }
      return list;
    };
    const stored: StoredResult = { summary, launchId: def.launch.id, def: t, attempts, startedAt: start };
    out.push(stored);
    resultsById.set(id, stored);
  }
  resultsByLaunch.set(def.launch.id, out);
  return out;
}

function liveResults(def: LaunchDef): StoredResult[] {
  const all = buildResults(def);
  if (!def.progress) return all;
  return all.slice(0, Math.floor(all.length * def.progress()));
}

export function launchView(def: LaunchDef): Launch {
  const results = liveResults(def);
  const c = counts(results.map((r) => r.summary));
  const l = { ...def.launch, counts: c };
  if (def.progress) {
    l.uploads = { queued: 1, processing: 1, failed: 0 };
  } else {
    const ups = uploadsOf(def);
    l.uploads = { queued: 0, processing: 0, failed: ups.filter((u) => u.status === 'failed').length };
  }
  return l;
}

export function projectLaunches(project: string): LaunchDef[] {
  return (launchOrder.get(project) ?? []).map((id) => launches.get(id)).filter((d): d is LaunchDef => !!d);
}

export function testKind(resultId: string): 'ui' | 'api' | null {
  let stored = resultsById.get(resultId);
  if (!stored) {
    for (const def of launches.values()) buildResults(def);
    stored = resultsById.get(resultId);
  }
  return stored?.def.kind ?? null;
}

export function launchDef(id: string): LaunchDef | undefined {
  return launches.get(id);
}

/** The id a reporter gives attempt `retry` (zero-based) of a result; log lines carry it. */
export function clientIdOf(resultId: string, retry: number): string {
  return `pw-mock-${resultId.slice(0, 8)}-${String(retry)}`;
}

export function launchResults(id: string): ResultSummary[] {
  const def = launches.get(id);
  if (!def) return [];
  return liveResults(def).map((r) => r.summary);
}

export function resultDetail(id: string): ResultDetail | undefined {
  let stored = resultsById.get(id);
  if (!stored) {
    for (const def of launches.values()) buildResults(def);
    stored = resultsById.get(id);
  }
  if (!stored) return undefined;
  const def = launches.get(stored.launchId);
  const rich = stored.launchId === RICH_LAUNCH && stored.def.index === 0;
  const history = def ? seriesHistory(def.project, stored.def, stored.launchId) : [];
  return {
    ...stored.summary,
    description: rich
      ? '<p>Pays for an order with a card saved in the customer profile.</p><ul><li>uses the <code>saved-card</code> fixture</li><li>compares the order summary with the approved screenshot</li></ul><p>See <a href="https://docs.example.com/checkout">the checkout spec</a>.</p><script>alert("x")</script>'
      : null,
    parameters: [...stored.def.parameters, ...(rich ? [{ name: 'viewport', value: '1280x720' }] : [])],
    links: rich
      ? [
          { name: 'SHOP-1234', url: 'https://issues.example.com/browse/SHOP-1234', type: 'issue' },
          { name: 'C-88', url: 'https://tms.example.com/case/C-88', type: 'tms' },
          { name: 'Checkout design', url: 'https://docs.example.com/design/checkout', type: 'link' },
        ]
      : [],
    attempts: stored.attempts(),
    history,
    launchId: stored.launchId,
    launchNumber: def?.launch.number ?? 0,
  };
}

function seriesHistory(project: string, t: TestDef, upTo: string): ResultDetail['history'] {
  const out: ResultDetail['history'] = [];
  let seen = false;
  for (const def of projectLaunches(project)) {
    if (def.launch.id === upTo) seen = true;
    if (!seen) continue;
    if (!def.tests.includes(t)) continue;
    const r = buildResults(def).find((x) => x.def === t);
    if (!r) continue;
    out.push({ launchId: def.launch.id, launchNumber: def.launch.number, status: r.summary.status, startedAt: def.launch.startedAt, resultId: r.summary.id });
    if (out.length >= 20) break;
  }
  return out;
}

export function testCaseHistory(project: string, testCaseId: string, branch?: string, environment?: string, limit = 100): TestCaseHistoryItem[] {
  const t = CATALOG.find((x) => x.testCaseId === testCaseId);
  if (!t) return [];
  const out: TestCaseHistoryItem[] = [];
  for (const def of projectLaunches(project)) {
    if (!def.tests.includes(t)) continue;
    if (branch && def.launch.vcs?.branch !== branch) continue;
    if (environment && def.launch.environment?.key !== environment) continue;
    const r = buildResults(def).find((x) => x.def === t);
    if (!r) continue;
    out.push({
      launchId: def.launch.id,
      launchNumber: def.launch.number,
      resultId: r.summary.id,
      status: r.summary.status,
      startedAt: iso(r.startedAt),
      durationMs: r.summary.durationMs,
      branch: def.launch.vcs?.branch ?? null,
      environment: def.launch.environment?.key ?? null,
    });
    if (out.length >= limit) break;
  }
  return out;
}

export function testCaseExists(testCaseId: string): boolean {
  return CATALOG.some((x) => x.testCaseId === testCaseId);
}

export function uploadsOf(def: LaunchDef): Upload[] {
  const created = new Date(def.launch.startedAt).getTime();
  const base = (i: number, extra: Partial<Upload>): Upload => ({
    id: uuid(`upload:${def.launch.id}:${String(i)}`),
    launchId: def.launch.id,
    format: 'allure',
    status: 'done',
    sizeBytes: 3_400_000 + i * 811_000,
    counts: { results: 0, attachments: 0, ignored: 0 },
    warnings: [],
    error: null,
    createdAt: iso(created + i * 3 * MIN),
    finishedAt: iso(created + i * 3 * MIN + 9000),
    ...extra,
  });
  const n = def.tests.length;
  if (def.progress) {
    return [
      base(0, { counts: { results: Math.floor(n * 0.35), attachments: 41, ignored: 0 } }),
      base(1, { format: 'playwright', status: 'processing', finishedAt: null, counts: { results: 0, attachments: 0, ignored: 0 } }),
      base(2, { format: 'auto', status: 'queued', finishedAt: null, sizeBytes: 1_200_000 }),
    ];
  }
  const list = [
    base(0, { counts: { results: Math.ceil(n / 2), attachments: 64, ignored: 3 }, warnings: ['3 files ignored: *-container.json without a matching result yet', 'executor.json: unknown field "reportName"'] }),
    base(1, { format: 'playwright', counts: { results: Math.floor(n / 2), attachments: 38, ignored: 0 } }),
  ];
  if (def.launch.number === 41) {
    list.push(
      base(2, {
        format: 'junit',
        status: 'failed',
        sizeBytes: 812_000_000,
        counts: { results: 0, attachments: 0, ignored: 0 },
        error: { code: 'limit_exceeded', message: 'The archive has more than 10000 entries', limit: '10000' },
      }),
    );
  }
  if (def.launch.status === 'interrupted') {
    list.push(base(3, { format: 'ctrf', status: 'failed', error: { code: 'invalid_report', message: 'results[12].status: unknown value "errored"' } }));
  }
  return list;
}

/** The label names of a label grouping (`epic,feature`, `label:feature`), or null for a preset. */
export function labelLevels(groupBy: TreeGroupBy): string[] | null {
  if (groupBy === 'suite' || groupBy === 'feature' || groupBy === 'package') return null;
  return groupBy.split(',').map((n) => n.trim().replace(/^label:/, ''));
}

/** The group path of a label grouping: `''` where a level's label is missing, trailing ones left out. */
export function labelPath(names: readonly string[], labels: readonly { name: string; value: string }[]): string[] {
  const values = names.map((n) => labels.find((l) => l.name === n)?.value || null);
  const last = values.findLastIndex((v) => v !== null);
  if (last < 0) return [''];
  return values.slice(0, last + 1).map((v) => v ?? '');
}

function groupPath(r: ResultSummary, def: TestDef, groupBy: TreeGroupBy): string[] {
  const levels = labelLevels(groupBy);
  if (levels) return labelPath(levels, r.labels);
  return groupBy === 'suite' ? r.suitePath : groupBy === 'feature' ? [def.feature, def.story] : def.pkg.split('.');
}

/** The environment of a result: the rich launch ran part of its UI tests in a second browser. */
export function resultEnv(r: ResultSummary): string | null {
  const stored = resultsById.get(r.id);
  const launch = launches.get(r.launchId)?.launch;
  if (stored && r.launchId === RICH_LAUNCH && stored.def.kind === 'ui' && stored.def.index % 3 === 0) return 'staging-firefox';
  return launch?.environment?.key ?? null;
}

/** The attachment kinds of a result, as the explorer filter names them. */
export function resultKinds(r: ResultSummary): string[] {
  const def = resultsById.get(r.id)?.def;
  if (!def || def.kind !== 'ui') return [];
  const out: string[] = [];
  if (r.status === 'failed' || r.status === 'broken') out.push('screenshot', 'trace');
  if (def.index % 7 === 0) out.push('video');
  return out;
}

function leafOf(r: ResultSummary): TreeTest {
  return {
    id: r.id,
    name: r.name,
    status: r.status,
    durationMs: r.durationMs,
    attemptCount: r.attemptCount > 1 ? r.attemptCount : null,
    flakyInRun: r.flakyInRun || undefined,
    flaky: r.flaky || undefined,
    muted: r.muted || undefined,
    transition: r.transition ?? null,
    defect: r.defect ? { typeId: r.defect.typeId, group: r.defect.group, name: r.defect.name, source: r.defect.source } : null,
  };
}

export interface TreeOptions {
  results?: ResultSummary[];
  includeTests?: boolean;
  reveal?: string | null;
  budget?: number;
}

/** The launch tree as the server builds it (groups, counts, durations, leaves within a budget). */
export function treeOf(launchId: string, groupBy: TreeGroupBy, o: TreeOptions = {}): TreeNode {
  const results = o.results ?? launchResults(launchId);
  const root: TreeNode = { name: 'root', path: [], counts: emptyCounts(), children: [] };
  const asked = labelLevels(groupBy);
  if (asked) {
    const levels = asked.filter((n) => results.some((r) => r.labels.some((l) => l.name === n && l.value)));
    groupBy = levels.join(',');
    root.levels = levels;
  }
  const byId = new Map<string, StoredResult>();
  for (const r of resultsByLaunch.get(launchId) ?? []) byId.set(r.summary.id, r);
  const direct = new Map<TreeNode, ResultSummary[]>();
  for (const r of results) {
    const def = byId.get(r.id)?.def;
    if (!def) continue;
    const path = groupPath(r, def, groupBy);
    let node = root;
    const nodes = [root];
    path.forEach((name, i) => {
      let child = node.children.find((c) => c.name === name);
      if (!child) {
        child = { name, path: path.slice(0, i + 1), counts: emptyCounts(), children: [] };
        node.children.push(child);
      }
      node = child;
      nodes.push(child);
    });
    if (o.includeTests) direct.set(node, [...(direct.get(node) ?? []), r]);
    else node.resultIds = [...(node.resultIds ?? []), r.id];
    for (const n of nodes) {
      n.counts.total++;
      n.counts[r.status]++;
      if (r.flakyInRun) n.counts.flaky++;
      if (r.muted) n.counts.muted++;
      n.durationMs = (n.durationMs ?? 0) + (r.durationMs ?? 0);
    }
  }
  if (o.includeTests) {
    const groups = [...direct.entries()].sort(
      ([a, x], [b, y]) =>
        Number(y.some((r) => r.id === o.reveal)) - Number(x.some((r) => r.id === o.reveal)) ||
        Number(y.some((r) => r.status === 'failed' || r.status === 'broken')) - Number(x.some((r) => r.status === 'failed' || r.status === 'broken')) ||
        x.length - y.length ||
        a.path.join('/').localeCompare(b.path.join('/')),
    );
    let left = o.budget ?? 5000;
    const walk = (n: TreeNode) => {
      n.testCount = direct.get(n)?.length ?? 0;
      n.children.forEach(walk);
    };
    walk(root);
    for (const [node, list] of groups) {
      if (list.length > left) continue;
      left -= list.length;
      node.tests = list.map(leafOf);
    }
  }
  return root;
}

/** The tests directly in one group, paged like the server pages them. */
export function treeTestsOf(launchId: string, groupBy: TreeGroupBy, results: ResultSummary[], path: string[]): TreeTest[] {
  const byId = new Map<string, StoredResult>();
  for (const r of resultsByLaunch.get(launchId) ?? []) byId.set(r.summary.id, r);
  const key = JSON.stringify(path);
  return results
    .filter((r) => {
      const def = byId.get(r.id)?.def;
      return def && JSON.stringify(groupPath(r, def, groupBy)) === key;
    })
    .sort((a, b) => (a.fullName < b.fullName ? -1 : a.fullName > b.fullName ? 1 : a.id < b.id ? -1 : 1))
    .map(leafOf);
}

export function projects(): Project[] {
  const p = (key: string, name: string, description: string | null, createdDaysAgo: number): Project => {
    const defs = projectLaunches(key);
    const last = defs[0];
    const lastView = last ? launchView(last) : null;
    return {
      key,
      name,
      description,
      createdAt: iso(NOW - createdDaysAgo * DAY),
      lastLaunchAt: lastView?.startedAt ?? null,
      launchCount: defs.length,
      lastLaunch: lastView ? { id: lastView.id, number: lastView.number, name: lastView.name, status: lastView.status, startedAt: lastView.startedAt, counts: lastView.counts } : null,
    };
  };
  return [
    p('web-shop', 'Web shop', 'Storefront end-to-end tests (Playwright) and shop API tests (JUnit 5).', 120),
    p('payments-api', 'Payments API', 'Contract tests of the payments and orders services.', 60),
    p('mobile-app', 'Mobile app', null, 3),
  ];
}

const ALL_PERMS = ['projects:read', 'projects:write', 'launches:read', 'launches:write', 'launches:rerun', 'results:triage', 'rules:write', 'settings:write', 'audit:read', 'tokens:read', 'tokens:write', 'cases:read', 'cases:write', 'cases:import', 'cases:sync', 'plans:write', 'runs:execute', 'dashboards:write'];

export type MockRole = 'admin' | 'viewer';

export function meFor(role: MockRole) {
  if (role === 'viewer') {
    return {
      name: 'viewer@example.com',
      kind: 'user',
      authMethod: 'session',
      roles: ['viewer'],
      groups: ['qa'],
      permissions: { global: [], scopes: { 'web-shop': ['projects:read', 'launches:read', 'tokens:read', 'cases:read'] }, allScopes: [] },
    };
  }
  return {
    name: 'alex@example.com',
    kind: 'user',
    authMethod: 'session',
    roles: ['admin'],
    groups: ['platform', 'qa'],
    permissions: { global: ALL_PERMS, scopes: {}, allScopes: [] },
  };
}

const ROLES: GrantableRole[] = [
  { name: 'viewer', builtin: true, description: 'Read projects and launches', permissions: ['projects:read', 'launches:read'], global: true, scopes: ['web-shop', 'payments-api', 'mobile-app'], allScopes: true },
  { name: 'uploader', builtin: true, description: 'Viewer plus create launches and upload results (CI)', permissions: ['projects:read', 'launches:read', 'launches:write'], global: true, scopes: ['web-shop', 'payments-api', 'mobile-app'], allScopes: true },
  { name: 'editor', builtin: true, description: 'Uploader plus triage and rules', permissions: ['projects:read', 'launches:read', 'launches:write', 'results:triage', 'rules:write'], global: true, scopes: ['web-shop', 'payments-api', 'mobile-app'], allScopes: true },
  { name: 'admin', builtin: true, description: 'Everything', permissions: ['*'], global: true, scopes: ['web-shop', 'payments-api', 'mobile-app'], allScopes: true },
  { name: 'release-manager', builtin: false, description: 'Custom role from the access file', permissions: ['launches:read', 'launches:write'], global: false, scopes: [], allScopes: false },
];

export const TOKEN_OPTIONS: TokenOptions = { maxDays: 365, defaultDays: 90, scopes: ['web-shop', 'payments-api', 'mobile-app'], roles: ROLES };

export function initialTokens(): TokenInfo[] {
  return [
    {
      id: uuid('token:ci-web-shop'),
      name: 'ci-web-shop',
      role: 'uploader',
      scopes: ['web-shop'],
      global: false,
      description: 'GitLab CI upload token',
      createdBy: 'alex@example.com',
      createdAt: iso(NOW - 40 * DAY),
      expiresAt: iso(NOW + 50 * DAY),
      lastUsedAt: iso(NOW - 2 * MIN),
      revokedAt: null,
      status: 'active',
    },
    {
      id: uuid('token:ci-payments'),
      name: 'ci-payments',
      role: 'uploader',
      scopes: ['payments-api'],
      global: false,
      createdBy: 'alex@example.com',
      createdAt: iso(NOW - 20 * DAY),
      expiresAt: iso(NOW + 70 * DAY),
      lastUsedAt: iso(NOW - DAY),
      revokedAt: null,
      status: 'active',
    },
    {
      id: uuid('token:dashboard-reader'),
      name: 'dashboard-reader',
      role: 'viewer',
      scopes: ['*'],
      global: false,
      description: 'Wall screen in the office',
      createdBy: 'alex@example.com',
      createdAt: iso(NOW - 200 * DAY),
      expiresAt: iso(NOW - 10 * DAY),
      lastUsedAt: iso(NOW - 11 * DAY),
      revokedAt: null,
      status: 'expired',
    },
    {
      id: uuid('token:old-admin'),
      name: 'old-admin',
      role: 'admin',
      scopes: [],
      global: true,
      createdBy: 'bootstrap',
      createdAt: iso(NOW - 90 * DAY),
      expiresAt: iso(NOW + 5 * DAY),
      lastUsedAt: null,
      revokedAt: iso(NOW - 30 * DAY),
      status: 'revoked',
    },
  ];
}

export function initialAudit(): AuditEvent[] {
  const actions: [string, string | undefined, unknown][] = [
    ['launch.create', 'web-shop', { launch: 42, externalId: 'gitlab-81294' }],
    ['upload.create', 'web-shop', { launch: 42, format: 'playwright', sizeBytes: 4211000 }],
    ['token.create', 'web-shop', { name: 'ci-web-shop', role: 'uploader', scopes: ['web-shop'] }],
    ['launch.finish', 'web-shop', { launch: 41, status: 'finished' }],
    ['project.update', 'payments-api', { fields: ['description'] }],
    ['token.revoke', undefined, { name: 'old-admin' }],
    ['project.create', 'mobile-app', { name: 'Mobile app' }],
    ['launch.delete', 'web-shop', { launch: 12 }],
    ['auth.login', undefined, { method: 'oidc' }],
  ];
  const out: AuditEvent[] = [];
  for (let i = 0; i < 160; i++) {
    const [action, scope, detail] = actions[i % actions.length] ?? ['auth.login', undefined, {}];
    out.push({
      id: 1000 - i,
      at: iso(NOW - i * 47 * MIN),
      principal: action.startsWith('upload') || action === 'launch.create' ? 'ci-web-shop' : i % 4 === 0 ? 'sam@example.com' : 'alex@example.com',
      principalKind: action.startsWith('upload') || action === 'launch.create' ? 'apiToken' : 'user',
      roles: action.startsWith('upload') || action === 'launch.create' ? 'uploader' : 'admin',
      scope,
      action,
      detail,
    });
  }
  return out;
}
