import { CSRF_HEADER } from '@franshub/ui-shell';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { server } from '../mocks/node';
import { ApiError, attachmentUrl, errorMessage, normalizeKind, request, setLoginRedirect, traceViewerUrl, withQuery } from './client';

afterEach(() => setLoginRedirect((url) => window.location.assign(url)));

describe('request', () => {
  it('turns an ErrorBody into an ApiError with code and details', async () => {
    server.use(http.get('/api/v1/x', () => HttpResponse.json({ code: 'forbidden', message: 'not allowed', details: { required: 'audit:read' } }, { status: 403 })));
    const err = await request('/api/v1/x').catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    const e = err as ApiError;
    expect(e.status).toBe(403);
    expect(e.code).toBe('forbidden');
    expect(errorMessage(e)).toBe('not allowed (requires audit:read)');
  });

  it('redirects to the login page on 401', async () => {
    const redirect = vi.fn();
    setLoginRedirect(redirect);
    server.use(http.get('/api/v1/y', () => HttpResponse.json({ code: 'unauthorized', message: 'authentication required' }, { status: 401 })));
    await expect(request('/api/v1/y')).rejects.toBeInstanceOf(ApiError);
    expect(redirect).toHaveBeenCalledWith(expect.stringMatching(/^\/auth\/login\?return=/));
  });

  it('sends JSON with the CSRF header on unsafe methods', async () => {
    let seen: Headers | null = null;
    server.use(
      http.post('/api/v1/z', async ({ request: req }) => {
        seen = req.headers;
        return HttpResponse.json(await req.json(), { status: 201 });
      }),
    );
    const out = await request<{ a: number }>('/api/v1/z', { method: 'POST', json: { a: 1 } });
    expect(out).toEqual({ a: 1 });
    expect((seen as Headers | null)?.get(CSRF_HEADER)).toBe('1');
    expect((seen as Headers | null)?.get('content-type')).toBe('application/json');
  });

  it('accepts 204 without a body', async () => {
    server.use(http.delete('/api/v1/w', () => new HttpResponse(null, { status: 204 })));
    await expect(request('/api/v1/w', { method: 'DELETE' })).resolves.toBeUndefined();
  });
});

describe('urls', () => {
  it('builds query strings without empty values', () => {
    expect(withQuery('/a', { x: 'b c', y: undefined, z: '', n: 3, f: false })).toBe('/a?x=b+c&n=3&f=false');
  });
  it('builds attachment and trace viewer links', () => {
    expect(attachmentUrl('web-shop', 'id1', true)).toBe('/api/v1/projects/web-shop/attachments/id1/content?download=1');
    const t = traceViewerUrl('web-shop', 'id1');
    expect(t.startsWith('/trace/index.html?trace=')).toBe(true);
    expect(decodeURIComponent(t.split('trace=')[1] ?? '')).toBe(`${window.location.origin}/api/v1/projects/web-shop/attachments/id1/content`);
  });
  it('normalizes attachment kinds, including snake_case', () => {
    expect(normalizeKind('image_diff')).toBe('imageDiff');
    expect(normalizeKind('httpExchange')).toBe('httpExchange');
    expect(normalizeKind('weird')).toBe('other');
  });
});

describe('messages for fix-wave error codes', () => {
  it('explain ai_self_review, export_busy and launch_finished in plain words', () => {
    expect(errorMessage(new ApiError(403, { code: 'ai_self_review', message: 'x', details: { required: 'cases:write' } }))).toMatch(/needs a second person/);
    expect(errorMessage(new ApiError(409, { code: 'export_busy', message: 'x' }))).toMatch(/at most 2 per person and 4 per project/);
    expect(errorMessage(new ApiError(409, { code: 'launch_finished', message: 'x' }))).toMatch(/The launch is finished/);
    expect(errorMessage(new ApiError(409, { code: 'other', message: 'kept' }))).toBe('kept');
  });
});
