// Sign-in with an API token: the token lives in sessionStorage (this tab only, gone when it
// closes), is sent as `Authorization: Bearer` on same-origin API calls and is never put in a URL.

const KEY = 'fastproof.apiToken';
export const SIGN_IN_PATH = '/sign-in';

export function getToken(): string | null {
  try {
    return window.sessionStorage.getItem(KEY);
  } catch {
    return null;
  }
}

export function setToken(token: string): boolean {
  try {
    window.sessionStorage.setItem(KEY, token);
    return true;
  } catch {
    return false;
  }
}

export function clearToken(): void {
  try {
    window.sessionStorage.removeItem(KEY);
  } catch {
    /* storage unavailable: nothing is stored */
  }
}

/** True while this tab is signed in with an API token (not a cookie session). */
export function usesToken(): boolean {
  return getToken() !== null;
}

/** A return path that stays on this site: relative, no `//`, no backslash. */
export function safeReturn(raw: string | null | undefined): string {
  if (!raw || raw.length > 2048 || !raw.startsWith('/') || raw.startsWith('//') || raw.includes('\\') || raw.startsWith(SIGN_IN_PATH)) return '/';
  return raw;
}

/** The sign-in page for a login URL (`/auth/login?return=...`) or a return path. */
export function signInUrl(from: string, reason?: 'expired'): string {
  let ret = from;
  if (from.startsWith('/auth/login')) {
    ret = new URL(from, window.location.origin).searchParams.get('return') ?? '/';
  }
  const q = new URLSearchParams({ return: safeReturn(ret) });
  if (reason) q.set('reason', reason);
  return `${SIGN_IN_PATH}?${q.toString()}`;
}

/**
 * Leaves the app for a login or logout target. A login sends the browser to the sign-in page
 * (where SSO and API tokens are offered) and forgets a token the server no longer accepts.
 */
export function navigateForAuth(url: string): void {
  if (url.startsWith('/auth/login')) {
    const expired = usesToken();
    clearToken();
    window.location.assign(signInUrl(url, expired ? 'expired' : undefined));
    return;
  }
  window.location.assign(url);
}

function sameOriginPath(input: RequestInfo | URL): string | null {
  try {
    const url = new URL(input instanceof Request ? input.url : String(input), window.location.href);
    return url.origin === window.location.origin ? url.pathname : null;
  } catch {
    return null;
  }
}

let installed = false;

/**
 * Wraps `fetch` so that, while a token is set, same-origin `/api/` calls carry it (and no cookies),
 * and `POST /auth/logout` signs out by forgetting it.
 */
export function installTokenAuth(): void {
  if (installed || typeof window === 'undefined') return;
  installed = true;
  const original = window.fetch.bind(window);
  window.fetch = (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const token = getToken();
    const path = token ? sameOriginPath(input) : null;
    if (!token || path === null) return original(input, init);
    if (path === '/auth/logout') {
      clearToken();
      return Promise.resolve(new Response('{}', { status: 200, headers: { 'Content-Type': 'application/json' } }));
    }
    if (!path.startsWith('/api/')) return original(input, init);
    const req = input instanceof Request ? new Request(input, init) : new Request(new URL(String(input), window.location.href), init);
    const headers = new Headers(req.headers);
    if (!headers.has('Authorization')) headers.set('Authorization', `Bearer ${token}`);
    return original(new Request(req, { headers, credentials: 'omit' }));
  };
}
