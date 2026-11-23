import { http, HttpResponse } from 'msw';
import type { LaunchMerge, LaunchMergeRequest } from '../api/merges';
import { launchDef } from './fixtures';

const API = '/api/v1';

type ErrorFn = (status: number, code: string, message: string) => Response;

const merges: LaunchMerge[] = [];

export function resetMerges(): void {
  merges.length = 0;
}

/** Launch merges over the fixture launches: a preview counts their results; a merge goes into
 *  `into` or, standing in for a new launch, the newest of them. Launches are not hidden. */
export function mergeHandlers(error: ErrorFn) {
  return [
    http.post(`${API}/projects/:key/launches/merge`, async ({ params, request }) => {
      const body = (await request.json()) as LaunchMergeRequest;
      const defs = body.launchIds.map((id) => launchDef(id));
      if (defs.length < 2 || defs.some((d) => !d || d.project !== params.key)) return error(404, 'not_found', 'launch not found');
      const launches = defs.flatMap((d) => (d ? [d.launch] : []));
      if (launches.some((l) => l.status === 'running' && l.id !== body.into)) return error(409, 'launch_running', 'finish the launches to merge first');
      const target = body.into ? launches.find((l) => l.id === body.into) : [...launches].sort((a, b) => b.number - a.number)[0];
      const sources = launches.filter((l) => l.id !== body.into);
      const attempts = sources.reduce((n, l) => n + l.counts.total, 0);
      const undoUntil = body.final ? null : new Date(Date.now() + 24 * 3600_000).toISOString();
      const m: LaunchMerge = {
        id: body.dryRun ? null : `mg-${String(merges.length + 1)}`,
        targetId: target?.id ?? null,
        targetNumber: target?.number ?? null,
        createdTarget: !body.into,
        sources: sources.map((l) => ({ id: l.id, number: l.number, name: l.name })),
        status: body.dryRun ? 'preview' : body.final ? 'final' : 'active',
        final: !!body.final,
        counts: { attempts, duplicates: 0, rekeyed: 0, results: attempts, sharedSeries: Math.min(...sources.map((l) => l.counts.total)), lateAttempts: 0 },
        createdBy: body.dryRun ? null : 'admin',
        createdAt: body.dryRun ? null : new Date().toISOString(),
        undoUntil,
        undoneAt: null,
        undoneBy: null,
        finalizedAt: null,
      };
      if (body.dryRun) return HttpResponse.json(m);
      merges.unshift(m);
      return HttpResponse.json(m, { status: 201 });
    }),
    http.get(`${API}/projects/:key/launches/:id/merges`, ({ params }) => {
      const id = String(params.id);
      return HttpResponse.json({ items: merges.filter((m) => m.targetId === id || m.sources.some((s) => s.id === id)) });
    }),
    http.post(`${API}/projects/:key/merges/:id/undo`, ({ params }) => {
      const m = merges.find((x) => x.id === params.id);
      if (!m) return error(404, 'not_found', 'merge not found');
      if (m.status !== 'active') return error(409, 'merge_not_active', `the merge is ${m.status}`);
      m.status = 'undone';
      m.undoneAt = new Date().toISOString();
      m.undoneBy = 'admin';
      return HttpResponse.json(m);
    }),
  ];
}
