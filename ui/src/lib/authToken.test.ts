import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { clearToken, getToken, installTokenAuth, safeReturn, setToken, signInUrl } from './authToken';

const seen: Request[] = [];

beforeAll(() => {
  vi.stubGlobal(
    'fetch',
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      seen.push(input instanceof Request ? new Request(input, init) : new Request(new URL(String(input), window.location.href), init));
      return Promise.resolve(new Response('{}', { status: 200 }));
    }),
  );
  installTokenAuth();
});

afterEach(() => {
  clearToken();
  seen.length = 0;
});

describe('API token sign-in', () => {
  it('keeps return paths on this site', () => {
    expect(safeReturn('/p/shop?branch=main')).toBe('/p/shop?branch=main');
    for (const bad of ['//evil.example.com', 'https://evil.example.com/', '/\\evil', '/sign-in?return=/x', null, '']) expect(safeReturn(bad)).toBe('/');
    expect(signInUrl('/auth/login?return=%2Fp%2Fshop')).toBe('/sign-in?return=%2Fp%2Fshop');
    expect(signInUrl('/auth/login?return=%2F%2Fevil.example.com', 'expired')).toBe('/sign-in?return=%2F&reason=expired');
  });

  it('adds the bearer token only to same-origin API calls and never to URLs', async () => {
    await fetch('/api/v1/projects');
    expect(seen[0]?.headers.get('Authorization')).toBeNull();
    expect(setToken('fp_secret_token_value')).toBe(true);
    await fetch('/api/v1/projects', { method: 'POST', body: '{}' });
    await fetch('https://elsewhere.example.com/api/v1/projects');
    await fetch('/assets/app.js');
    expect(seen[1]?.headers.get('Authorization')).toBe('Bearer fp_secret_token_value');
    expect(seen[1]?.credentials).toBe('omit');
    expect(seen[1]?.url).not.toContain('fp_secret_token_value');
    expect(seen[2]?.headers.get('Authorization')).toBeNull();
    expect(seen[3]?.headers.get('Authorization')).toBeNull();
  });

  it('signs out by forgetting the token without calling the server', async () => {
    setToken('fp_secret_token_value');
    const res = await fetch('/auth/logout', { method: 'POST' });
    expect(res.ok).toBe(true);
    expect(getToken()).toBeNull();
    expect(seen).toHaveLength(0);
  });
});
