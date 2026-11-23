import { http, HttpResponse } from 'msw';
import type { CreateTrackerIssue, LinkTrackerIssue, TrackerIssue } from '../api/trackers';
import type { MockRole } from './fixtures';
import { NOW } from './fixtures';
import { settingsOf, triage } from './stageC';

const API = '/api/v1';
const DAY = 86_400_000;
const iso = (ms: number) => new Date(ms).toISOString();

interface Helpers {
  role: () => MockRole;
  error: (status: number, code: string, message: string, details?: unknown) => Response;
  audit: (action: string, scope: string | undefined, detail: unknown) => void;
}

function seed(): TrackerIssue[] {
  return [
    {
      id: '0193a000-0000-7000-8000-000000001180',
      trackerId: 'jira',
      trackerType: 'jira',
      key: 'SHOP-1180',
      url: 'https://issues.example.com/browse/SHOP-1180',
      title: 'Cart badge shows the old count after adding an item',
      state: 'Done',
      category: 'closed',
      createdBy: 'sam@example.com',
      createdAt: iso(NOW - 9 * DAY),
      updatedAt: iso(NOW - 2 * 3600_000),
      closedAt: iso(NOW - 2 * 3600_000),
      syncedAt: iso(NOW - 600_000),
      nextSyncAt: iso(NOW + DAY),
      syncError: null,
      unmuteSuggested: true,
    },
    {
      id: '0193a000-0000-7000-8000-000000001234',
      trackerId: 'jira',
      trackerType: 'jira',
      key: 'SHOP-1234',
      url: 'https://issues.example.com/browse/SHOP-1234',
      title: 'Checkout returns 500 when the voucher is expired',
      state: 'In Progress',
      category: 'open',
      createdBy: 'alex@example.com',
      createdAt: iso(NOW - 3 * DAY),
      updatedAt: iso(NOW - DAY),
      closedAt: null,
      syncedAt: iso(NOW - 240_000),
      nextSyncAt: iso(NOW + 360_000),
      syncError: null,
      unmuteSuggested: false,
    },
    {
      id: '0193a000-0000-7000-8000-000000000042',
      trackerId: 'jira',
      trackerType: 'jira',
      key: 'OPS-77',
      url: 'https://issues.example.com/browse/OPS-77',
      title: 'Test database is overloaded at night',
      state: 'Open',
      category: 'open',
      createdBy: 'rule',
      createdAt: iso(NOW - 20 * DAY),
      updatedAt: iso(NOW - 20 * DAY),
      closedAt: null,
      syncedAt: iso(NOW - 3 * 3600_000),
      nextSyncAt: iso(NOW + 600_000),
      syncError: 'the tracker is rate limiting; try again in 60s',
      unmuteSuggested: false,
    },
  ];
}

let issues: TrackerIssue[] = seed();
let seq = 1300;
const requests = new Map<string, string>();

export function resetTrackers(): void {
  issues = seed();
  seq = 1300;
  requests.clear();
}

export function trackerHandlers({ role, error, audit }: Helpers) {
  const triager = () => role() === 'admin';
  const connected = (project: string, id: string) => settingsOf(project).issueTrackers.find((t) => t.id === id && t.api);
  return [
    http.get(`${API}/projects/:key/tracker-issues`, ({ request }) => {
      const u = new URL(request.url);
      const state = u.searchParams.get('state');
      const suggested = u.searchParams.get('unmuteSuggested');
      const items = issues.filter((i) => (!state || i.category === state) && (suggested === null || String(i.unmuteSuggested) === suggested));
      return HttpResponse.json({ items, next: null });
    }),
    http.post(`${API}/projects/:key/tracker-issues/lookup`, async ({ request }) => {
      const body = (await request.json()) as {
        urls?: string[];
        keys?: string[];
      };
      const items = issues.filter((i) => (i.url && body.urls?.includes(i.url)) || (i.key && body.keys?.includes(i.key)));
      return HttpResponse.json({ items, next: null });
    }),
    http.post(`${API}/projects/:key/tracker-issues/link`, async ({ params, request }) => {
      if (!triager())
        return error(403, 'forbidden', 'requires permission results:triage', {
          required: 'results:triage',
        });
      const body = (await request.json()) as LinkTrackerIssue;
      const t = connected(String(params.key), body.tracker);
      if (!t) return error(409, 'no_issue_tracker', `no issue tracker with id "${body.tracker}" and an api connection is configured`);
      const key = body.issueKey.trim();
      const now = new Date().toISOString();
      const issue: TrackerIssue = issues.find((i) => i.key === key) ?? {
        id: `0193a000-0000-7000-8000-${String(++seq).padStart(12, '0')}`,
        trackerId: body.tracker,
        trackerType: t.type,
        key,
        url: t.urlTemplate.replace('{key}', key),
        title: 'Existing issue',
        state: 'Open',
        category: 'open',
        createdBy: 'alex@example.com',
        createdAt: now,
        updatedAt: now,
        closedAt: null,
        syncedAt: now,
        nextSyncAt: now,
        syncError: null,
        unmuteSuggested: false,
      };
      if (!issues.includes(issue)) issues.unshift(issue);
      const n = triage(String(params.key), {
        resultIds: body.resultIds,
        launchId: body.launchId,
        fingerprint: body.fingerprint,
        issue: { url: issue.url, key: issue.key },
      });
      audit('tracker.issue.link', String(params.key), { key });
      return HttpResponse.json({
        issue,
        preview: {
          title: issue.title,
          body: '',
          markup: 'adf',
          text: '',
          screenshots: [],
        },
        attached: 0,
        notes: [],
        updatedResults: n,
        repeated: false,
      });
    }),
    http.post(`${API}/projects/:key/tracker-issues`, async ({ params, request }) => {
      if (!triager())
        return error(403, 'forbidden', 'requires permission results:triage', {
          required: 'results:triage',
        });
      const body = (await request.json()) as CreateTrackerIssue;
      const t = connected(String(params.key), body.tracker);
      if (!t) return error(409, 'no_issue_tracker', `no issue tracker with id "${body.tracker}" and an api connection is configured`);
      const count = body.resultIds?.length ?? 3;
      const title = body.title ?? 'checkout pays with a saved card: expected 200 but was 500';
      const text = [
        ...(body.note ? [body.note, ''] : []),
        'Test: shop.Checkout#paysWithSavedCard',
        'Status: failed',
        'Launch: #42 nightly https://fastproof.example.com/p/web-shop/launches/42',
        'Branch: main',
        `Results with this failure in the launch: ${String(count)}`,
        '',
        'expected 200 but was 500',
        '',
        'AssertionError: expected 200 but was 500\n    at checkout.spec.ts:41',
        '',
        'Created from FastProof by alex@example.com.',
      ].join('\n');
      const preview = {
        title,
        body: text,
        markup: 'adf',
        text,
        screenshots: body.attachScreenshots ? ['checkout-failure.png'] : [],
      };
      if (body.dryRun)
        return HttpResponse.json({
          issue: null,
          preview,
          attached: 0,
          notes: [],
          updatedResults: 0,
          repeated: false,
        });
      const prev = requests.get(body.requestId);
      const old = prev ? issues.find((i) => i.id === prev) : undefined;
      if (old)
        return HttpResponse.json({
          issue: old,
          preview,
          attached: 0,
          notes: [],
          updatedResults: 0,
          repeated: true,
        });
      const n = ++seq;
      const key = `SHOP-${String(n)}`;
      const now = new Date().toISOString();
      const issue: TrackerIssue = {
        id: `0193a000-0000-7000-8000-${String(n).padStart(12, '0')}`,
        trackerId: body.tracker,
        trackerType: t.type,
        key,
        url: t.urlTemplate.replace('{key}', key),
        title,
        state: 'To Do',
        category: 'open',
        createdBy: 'alex@example.com',
        createdAt: now,
        updatedAt: now,
        closedAt: null,
        syncedAt: now,
        nextSyncAt: now,
        syncError: null,
        unmuteSuggested: false,
      };
      issues.unshift(issue);
      requests.set(body.requestId, issue.id);
      const updated = triage(String(params.key), {
        resultIds: body.resultIds,
        launchId: body.launchId,
        fingerprint: body.fingerprint,
        issue: { url: issue.url, key },
      });
      audit('tracker.issue.create', String(params.key), { key });
      return HttpResponse.json({
        issue,
        preview,
        attached: preview.screenshots.length,
        notes: [],
        updatedResults: updated,
        repeated: false,
      });
    }),
    http.get(`${API}/projects/:key/tracker-issues/:id`, ({ params }) => {
      const i = issues.find((x) => x.id === params.id);
      if (!i) return error(404, 'not_found', 'tracker issue not found');
      return HttpResponse.json({
        ...i,
        links: [],
        mutedTests: i.unmuteSuggested ? 1 : 0,
        canUnmute: triager(),
      });
    }),
    http.delete(`${API}/projects/:key/tracker-issues/:id`, ({ params }) => {
      if (!triager())
        return error(403, 'forbidden', 'requires permission results:triage', {
          required: 'results:triage',
        });
      issues = issues.filter((x) => x.id !== params.id);
      return new HttpResponse(null, { status: 204 });
    }),
    http.post(`${API}/projects/:key/tracker-issues/:id/sync`, ({ params }) => {
      const i = issues.find((x) => x.id === params.id);
      if (!i) return error(404, 'not_found', 'tracker issue not found');
      i.syncedAt = new Date().toISOString();
      i.syncError = null;
      return HttpResponse.json({ ...i, canUnmute: triager() });
    }),
    http.post(`${API}/projects/:key/tracker-issues/:id/unmute`, ({ params }) => {
      if (!triager())
        return error(403, 'forbidden', 'requires permission results:mute', {
          required: 'results:mute',
        });
      const i = issues.find((x) => x.id === params.id);
      if (!i) return error(404, 'not_found', 'tracker issue not found');
      i.unmuteSuggested = false;
      audit('tracker.issue.unmute', String(params.key), { issueId: i.id });
      return HttpResponse.json({ tests: 1, results: 1 });
    }),
    http.post(`${API}/projects/:key/tracker-issues/:id/dismiss`, ({ params }) => {
      if (!triager())
        return error(403, 'forbidden', 'requires permission results:triage', {
          required: 'results:triage',
        });
      const i = issues.find((x) => x.id === params.id);
      if (!i) return error(404, 'not_found', 'tracker issue not found');
      i.unmuteSuggested = false;
      return new HttpResponse(null, { status: 204 });
    }),
    http.post(`${API}/projects/:key/issue-trackers/:id/test`, ({ params }) => {
      if (!triager())
        return error(403, 'forbidden', 'requires permission settings:write', {
          required: 'settings:write',
        });
      const t = connected(String(params.key), String(params.id));
      if (!t) return error(409, 'no_issue_tracker', 'no issue tracker with this id and an api connection is configured');
      return HttpResponse.json({
        ok: true,
        project: t.api?.project ?? null,
        error: null,
      });
    }),
  ];
}
