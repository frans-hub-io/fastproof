import type { TrackerApi } from '../api/trackers';
import type { IssueTracker } from '../api/types-next';

const TRACKER_ID = /^[a-z0-9][a-z0-9_-]{0,31}$/;
const HTTP_URL = /^https?:\/\/[^\s/?#]+[^\s?#]*$/i;

/** A request id that makes a create idempotent (a retried click creates nothing twice). */
export function newRequestId(): string {
  const c: Crypto = globalThis.crypto;
  if (typeof c.randomUUID === 'function') return c.randomUUID();
  const b = Array.from(c.getRandomValues(new Uint8Array(16)));
  b[6] = ((b[6] ?? 0) & 0x0f) | 0x40;
  b[8] = ((b[8] ?? 0) & 0x3f) | 0x80;
  const h = b.map((x) => x.toString(16).padStart(2, '0')).join('');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/** Trackers with an API connection: issues can be created in them. */
export function connectedTrackers(trackers: IssueTracker[] | undefined): (IssueTracker & { id: string })[] {
  return (trackers ?? []).filter((t): t is IssueTracker & { id: string } => !!t.id && !!t.api);
}

/** Problems of the API part of a tracker, by field. */
export function trackerApiErrors(t: IssueTracker): Record<string, string> {
  const e: Record<string, string> = {};
  const a = t.api;
  if (!a) return e;
  if (!t.id || !TRACKER_ID.test(t.id)) e.id = 'Lowercase letters, digits, - and _ (up to 32)';
  if (!HTTP_URL.test(a.url)) e.url = 'An http(s) URL without query';
  if (!a.tokenSecretRef) e.token = 'Choose the secret with the token (purpose tracker)';
  const p = a.project.trim();
  if (t.type === 'jira' && !/^[A-Z][A-Z0-9_]{0,31}$/.test(p)) e.project = 'A Jira project key such as SHOP';
  if (t.type === 'github' && !/^[A-Za-z0-9._-]+\/[A-Za-z0-9._-]+$/.test(p)) e.project = 'owner/repo';
  if (t.type === 'gitlab' && !/^(\d+|[A-Za-z0-9._-]+(\/[A-Za-z0-9._-]+)+)$/.test(p)) e.project = 'group/project or the numeric id';
  if (t.type === 'azure' && (!p || /[/\\?#%]/.test(p))) e.project = 'The project name';
  if ((a.sync ?? 'poll') === 'webhook' && !a.webhookSecretRef) e.webhook = 'Choose the webhook secret (purpose webhook)';
  return e;
}

/** Drops empty optional fields before saving. */
export function cleanApi(t: IssueTracker): IssueTracker {
  if (!t.api) {
    return { ...t, api: null };
  }
  const a = t.api;
  const api: TrackerApi = {
    url: a.url.trim(),
    tokenSecretRef: a.tokenSecretRef,
    project: a.project.trim(),
  };
  if (t.type === 'jira' && a.deployment === 'server') api.deployment = 'server';
  if (a.auth) api.auth = a.auth;
  if (a.issueType?.trim() && (t.type === 'jira' || t.type === 'azure')) api.issueType = a.issueType.trim();
  if (a.labels?.length) api.labels = a.labels;
  if (a.sync && a.sync !== 'poll') api.sync = a.sync;
  if (a.sync === 'webhook' && a.webhookSecretRef) api.webhookSecretRef = a.webhookSecretRef;
  if (a.attachScreenshots && t.type !== 'github') api.attachScreenshots = true;
  if (a.titleTemplate?.trim()) api.titleTemplate = a.titleTemplate.trim();
  if (a.bodyTemplate?.trim()) api.bodyTemplate = a.bodyTemplate;
  if (t.type === 'azure' && a.closedStates?.length) api.closedStates = a.closedStates;
  return { ...t, api };
}

function canonical(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(canonical).join(',')}]`;
  if (v && typeof v === 'object') {
    const o = v as Record<string, unknown>;
    return `{${Object.keys(o)
      .filter((k) => o[k] !== null && o[k] !== undefined && k !== 'redacted')
      .sort()
      .map((k) => `${JSON.stringify(k)}:${canonical(o[k])}`)
      .join(',')}}`;
  }
  return JSON.stringify(v ?? null);
}

/** Whether the API settings of a tracker are saved as they are edited (the connection test uses the saved ones). */
export function apiSaved(saved: IssueTracker[], t: IssueTracker): boolean {
  const s = saved.find((x) => x.id === t.id);
  return !!s && canonical(cleanApi(s).api) === canonical(cleanApi(t).api);
}
