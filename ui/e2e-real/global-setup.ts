import { execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { ADMIN_TOKEN, BASE_URL, BIN, ENABLED, FIXTURES, PROJECT, WORK_DIR, adminTokensFile, saveSeeded } from './env';

async function api(method: string, path: string, body?: unknown, contentType = 'application/json'): Promise<{ status: number; body: unknown }> {
  const res = await fetch(`${BASE_URL}${path}`, {
    method,
    headers: { Authorization: `Bearer ${ADMIN_TOKEN}`, Accept: 'application/json', ...(body === undefined ? {} : { 'Content-Type': contentType }) },
    body: body === undefined ? undefined : typeof body === 'string' ? body : JSON.stringify(body),
  });
  const text = await res.text();
  return { status: res.status, body: text ? (JSON.parse(text) as unknown) : null };
}

async function waitReady(): Promise<void> {
  const until = Date.now() + 120_000;
  for (;;) {
    try {
      const r = await fetch(`${BASE_URL}/healthz`);
      if (r.ok) return;
    } catch {
      /* not up yet */
    }
    if (Date.now() > until) throw new Error(`${BASE_URL} did not become ready`);
    await new Promise((r) => setTimeout(r, 500));
  }
}

function upload(path: string, launch: string, name: string, branch: string, environment?: string): void {
  const tokenFile = join(WORK_DIR, 'admin-token');
  writeFileSync(tokenFile, ADMIN_TOKEN);
  const args = ['upload', join(FIXTURES, path), '--launch', launch, '--name', name, '--branch', branch, '--finish', '--timeout', '300'];
  if (environment) args.push('--environment', environment);
  execFileSync(BIN, args, {
    env: { PATH: process.env.PATH ?? '', FASTPROOF_URL: BASE_URL, FASTPROOF_TOKEN_FILE: tokenFile, FASTPROOF_PROJECT: PROJECT },
    stdio: ['ignore', 'pipe', 'inherit'],
  });
}

interface LaunchView {
  id: string;
  name: string;
  status: string;
}

interface ResultView {
  id: string;
  testCaseId: string;
  status: string;
}

/** A launch that is still running, with results and live log lines sent like a reporter does. */
async function liveLaunch(): Promise<string> {
  const run = randomBytes(6).toString('hex');
  const l = ((await api('POST', `/api/v1/projects/${PROJECT}/launches`, {
    externalId: `e2e-live-${run}`,
    name: 'checkout live run',
    vcs: { repository: 'https://git.example.com/shop/web', branch: 'main', commit: 'a1b2c3d4e5f60718293a4b5c6d7e8f9012345678' },
    environment: { key: 'staging', values: { browser: 'chromium' } },
  })) as { status: number; body: LaunchView });
  if (l.status >= 300) throw new Error(`starting the live launch: ${String(l.status)}`);
  const now = Date.now();
  const id = (t: string, retry: number) => `e2e-${run}-${t}-${String(retry)}`;
  const results = [
    { id: id('cart', 0), name: 'adds an item to the cart', fullName: 'shop.cart.adds_an_item', status: 'passed', start: now - 9000, stop: now - 8000, retry: 0 },
    { id: id('pay', 0), name: 'pays with a saved card', fullName: 'shop.checkout.pays_with_saved_card', status: 'failed', start: now - 7000, stop: now - 5000, retry: 0, error: { message: 'Timeout 5000ms exceeded waiting for #pay', trace: 'at checkout.spec.ts:42' } },
    { id: id('login', 0), name: 'logs in', fullName: 'shop.auth.logs_in', status: 'failed', start: now - 6000, stop: now - 5500, retry: 0, error: { message: 'expected 200, got 503' } },
    { id: id('login', 1), name: 'logs in', fullName: 'shop.auth.logs_in', status: 'passed', start: now - 5000, stop: now - 4500, retry: 1 },
    { id: id('search', 0), name: 'searches the catalog', fullName: 'shop.catalog.searches', status: 'running', start: now - 1000, retry: 0 },
  ];
  const r = ((await api('POST', `/api/v1/projects/${PROJECT}/launches/${l.body.id}/results`, results.map((x) => JSON.stringify(x)).join('\n'), 'application/x-ndjson')) as { status: number; body: { accepted: number } });
  if (r.status !== 202) throw new Error(`posting live results: ${String(r.status)} ${JSON.stringify(r.body)}`);
  const lines: object[] = [];
  const levels = ['info', 'debug', 'warn', 'error', 'info'] as const;
  for (const [n, rid] of [id('pay', 0), id('login', 0), id('search', 0)].entries()) {
    for (let seq = 0; seq < 40; seq++) {
      lines.push({ resultId: rid, attempt: 0, seq, ts: now - 6000 + n * 100 + seq * 50, level: levels[seq % levels.length], stream: seq % 7 === 0 ? 'step' : 'stdout', message: `\u001b[36m${rid.split('-')[2] ?? ''}\u001b[0m line ${String(seq)}`, step: seq % 7 === 0 ? 'checkout › pay' : null });
    }
  }
  const logs = await api('POST', `/api/v1/projects/${PROJECT}/launches/${l.body.id}/logs`, lines.map((x) => JSON.stringify(x)).join('\n'), 'application/x-ndjson');
  if (logs.status !== 202) throw new Error(`posting live logs: ${String(logs.status)}`);
  return l.body.id;
}

export default async function globalSetup(): Promise<void> {
  if (!ENABLED) return;
  adminTokensFile();
  await waitReady();
  const p = await api('POST', '/api/v1/projects', { key: PROJECT, name: 'Web shop', description: 'Storefront end-to-end tests' });
  if (p.status !== 201 && p.status !== 409) throw new Error(`creating the project: ${String(p.status)}`);
  const v = ((await api('POST', '/api/v1/tokens', { name: `e2e-viewer-${randomBytes(4).toString('hex')}`, role: 'viewer', scopes: [PROJECT], expiresInDays: 1 })) as { status: number; body: { token: string } });
  if (v.status !== 201) throw new Error(`creating the viewer token: ${String(v.status)}`);

  const existing = ((await api('GET', `/api/v1/projects/${PROJECT}/launches?limit=50`)) as { status: number; body: { items: LaunchView[] } });
  if (!existing.body.items.some((l) => l.name === 'nightly 3')) {
    upload('pytest/allure-results', 'e2e-nightly-1', 'nightly 1', 'main');
    upload('playwright/allure-results', 'e2e-pw-1', 'playwright 1', 'main');
    upload('pytest/allure-results', 'e2e-nightly-2', 'nightly 2', 'main');
    upload('junit5/allure-results', 'e2e-junit-1', 'junit 1', 'feature/checkout');
    upload('pytest/allure-results', 'e2e-feature-1', 'feature 1', 'feature/checkout');
    upload('pytest/allure-results', 'e2e-nightly-3', 'nightly 3', 'main');
  }
  const runningLaunchId = await liveLaunch();
  const all = ((await api('GET', `/api/v1/projects/${PROJECT}/launches?limit=50`)) as { status: number; body: { items: LaunchView[] } });
  const launches = Object.fromEntries(all.body.items.map((l) => [l.name, l.id]));
  const nightly = launches['nightly 3'];
  if (!nightly) throw new Error('nightly 3 is missing');
  const failed = ((await api('GET', `/api/v1/projects/${PROJECT}/launches/${nightly}/results?status=failed&limit=5`)) as { status: number; body: { items: ResultView[] } });
  const first = failed.body.items[0];
  if (!first) throw new Error('nightly 3 has no failed result');
  const live = ((await api('GET', `/api/v1/projects/${PROJECT}/launches/${runningLaunchId}/results?status=failed&limit=5`)) as { status: number; body: { items: (ResultView & { name: string })[] } });
  const pay = live.body.items.find((r) => r.name === 'pays with a saved card');
  if (!pay) throw new Error('the live launch has no failed payment result');
  saveSeeded({ viewerToken: v.body.token, launches, failedResultId: first.id, testCaseId: first.testCaseId, runningLaunchId, liveResultId: pay.id });
}
