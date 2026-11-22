import { describe, expect, it } from 'vitest';
import { ApiError, projectPath } from './client';
import { fetchAllFolders, getVersioned, PreconditionFailedError, revisionOf, writeVersioned, type Case, type CaseWrite } from './tm';
import { touchCase } from '../mocks/tm';
import { server } from '../mocks/node';

const path = `${projectPath('web-shop')}/cases/1`;

function write(c: Case, name: string): CaseWrite {
  return { name, folderId: c.folder?.id ?? null, status: c.status, body: c.body, fields: c.fields, tags: c.tags };
}

describe('ETag conflict flow', () => {
  it('reads the ETag and saves with If-Match', async () => {
    const v = await getVersioned<Case>(path);
    expect(v.etag).toBe(`"r${String(v.value.revision)}"`);
    expect(revisionOf(v.etag)).toBe(v.value.revision);
    const saved = await writeVersioned<Case>(path, 'PUT', v.etag, write(v.value, 'Pay with a saved card (edited)'));
    expect(saved.value.name).toBe('Pay with a saved card (edited)');
    expect(saved.value.revision).toBe(v.value.revision + 1);
    expect(saved.etag).toBe(`"r${String(saved.value.revision)}"`);
  });

  it('a stale ETag gives a typed 412 with the details', async () => {
    const v = await getVersioned<Case>(path);
    touchCase(1, 'code', 'ci');
    const err = await writeVersioned<Case>(path, 'PUT', v.etag, write(v.value, 'mine')).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(PreconditionFailedError);
    const c = err as PreconditionFailedError;
    expect(c.status).toBe(412);
    expect(c.code).toBe('precondition_failed');
    expect(c.conflict).toEqual({ currentRevision: v.value.revision + 1, changedBy: 'ci', source: 'code' });
    const fresh = await getVersioned<Case>(path);
    const retried = await writeVersioned<Case>(path, 'PUT', fresh.etag, write(fresh.value, 'mine'));
    expect(retried.value.name).toBe('mine');
  });

  it('a write without If-Match is refused with 428', async () => {
    const v = await getVersioned<Case>(path);
    const res = await fetch(path, { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(write(v.value, 'x')) });
    expect(res.status).toBe(428);
    const err = await writeVersioned<Case>(path, 'PUT', '', write(v.value, 'x')).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).not.toBeInstanceOf(PreconditionFailedError);
    expect((err as ApiError).status).toBe(428);
  });

  it('code-owned fields of an automated case are refused', async () => {
    const list = (await (await fetch(`${projectPath('web-shop')}/cases?automation=automated&limit=1`)).json()) as { items: { number: number }[] };
    const n = list.items[0]?.number ?? 0;
    const v = await getVersioned<Case>(`${projectPath('web-shop')}/cases/${String(n)}`);
    expect(v.value.codeOwned).toContain('name');
    const err = await writeVersioned<Case>(`${projectPath('web-shop')}/cases/${String(n)}`, 'PUT', v.etag, write(v.value, 'renamed')).catch((e: unknown) => e);
    expect((err as ApiError).status).toBe(409);
    expect((err as ApiError).code).toBe('field_from_code');
  });
});

describe('folder tree', () => {
  it('reads every folder in one request, parents first', async () => {
    const urls: string[] = [];
    const seen = ({ request }: { request: Request }) => {
      if (request.url.includes('/case-folders')) urls.push(request.url);
    };
    server.events.on('request:start', seen);
    try {
      const folders = await fetchAllFolders('web-shop');
      expect(urls).toHaveLength(1);
      expect(new URL(urls[0] ?? '').searchParams.get('all')).toBe('true');
      expect(folders.some((f) => f.parentId !== null)).toBe(true);
      const at = new Map(folders.map((f, i) => [f.id, i]));
      for (const [i, f] of folders.entries()) if (f.parentId) expect(at.get(f.parentId) ?? Infinity).toBeLessThan(i);
    } finally {
      server.events.removeListener('request:start', seen);
    }
  });
});
