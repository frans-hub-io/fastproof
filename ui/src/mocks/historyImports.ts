import { http, HttpResponse } from 'msw';
import type { HistoryImport } from '../api/historyImports';

const API = '/api/v1';

type ErrorFn = (status: number, code: string, message: string) => Response;

const imports: HistoryImport[] = [];
const polls = new Map<string, number>();

export function resetHistoryImports(): void {
  imports.length = 0;
  polls.clear();
}

/** An import that ends after two reads of the list, so the page shows it running first. */
export function historyImportHandlers(error: ErrorFn) {
  return [
    http.get(`${API}/projects/:key/history-imports`, () => {
      for (const i of imports) {
        if (i.status === 'done' || i.status === 'failed') continue;
        const n = (polls.get(i.id) ?? 0) + 1;
        polls.set(i.id, n);
        if (n === 1) Object.assign(i, { status: 'running', pointsTotal: 3, pointsDone: 1, launchesCreated: 1, resultsImported: 5 });
        if (n >= 2) {
          const bad = i.environment === 'broken';
          Object.assign(i, bad
            ? { status: 'failed', error: 'history.json is not an Allure 2 history: expected value at line 1 column 1', finishedAt: new Date().toISOString() }
            : { status: 'done', pointsTotal: 3, pointsDone: 3, launchesCreated: 3, resultsImported: 15, resultsUnmatched: 2, warnings: ['2 results belong to tests this project does not know and the history does not name them; import after the first live run to join them'], finishedAt: new Date().toISOString() });
        }
      }
      return HttpResponse.json({ items: imports });
    }),
    http.post(`${API}/projects/:key/history-imports`, ({ request }) => {
      const url = new URL(request.url);
      const source = url.searchParams.get('source');
      if (source !== 'allure2' && source !== 'allure3') return error(400, 'bad_request', 'source must be allure2 or allure3');
      if (!(request.headers.get('content-type') ?? '').startsWith('multipart/form-data')) return error(400, 'bad_request', 'the multipart body has no non-empty `history` field');
      if (imports.some((i) => i.status === 'queued' || i.status === 'running')) return error(409, 'history_import_busy', 'an import of this project is queued or running');
      const i: HistoryImport = {
        id: `hi-${String(imports.length + 1)}`,
        source,
        branch: url.searchParams.get('branch') || null,
        environment: url.searchParams.get('environment') || null,
        status: 'queued',
        pointsTotal: 0,
        pointsDone: 0,
        launchesCreated: 0,
        resultsImported: 0,
        resultsExisting: 0,
        resultsUnmatched: 0,
        resultsSkipped: 0,
        warnings: [],
        createdBy: 'admin',
        createdAt: new Date().toISOString(),
        finishedAt: null,
      };
      imports.unshift(i);
      return HttpResponse.json(i, { status: 202 });
    }),
  ];
}
