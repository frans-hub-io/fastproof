import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { projectPath, request, withQuery } from './client';
import type { components } from './schema.gen';

export type HistoryImport = components['schemas']['HistoryImport'];
export type HistorySource = 'allure2' | 'allure3';

const MIB = 1024 * 1024;

/** The multipart field and the largest size of each history file the server accepts. */
export const ALLURE2_FILES: Record<string, { field: string; max: number }> = {
  'history.json': { field: 'history', max: 32 * MIB },
  'history-trend.json': { field: 'historyTrend', max: 4 * MIB },
  'duration-trend.json': { field: 'durationTrend', max: 4 * MIB },
  'categories-trend.json': { field: 'categoriesTrend', max: 4 * MIB },
  'retry-trend.json': { field: 'retryTrend', max: 4 * MIB },
};
export const ALLURE3_MAX = 256 * MIB;

export const historyKeys = {
  list: (key: string) => ['history-imports', key] as const,
};

export function isActive(i: HistoryImport): boolean {
  return i.status === 'queued' || i.status === 'running';
}

/** The newest imports of a project; polled while one is queued or running. */
export function useHistoryImports(key: string) {
  return useQuery({
    queryKey: historyKeys.list(key),
    queryFn: () => request<{ items: HistoryImport[] }>(`${projectPath(key)}/history-imports`).then((r) => r.items),
    refetchInterval: (q) => (q.state.data?.some(isActive) ? 2_000 : false),
  });
}

/** Checks the chosen files and maps them to multipart fields; an error text when they do not fit. */
export function planFiles(source: HistorySource, files: File[]): { form: FormData } | { error: string } {
  if (files.length === 0) return { error: 'Choose the history files.' };
  const form = new FormData();
  if (source === 'allure3') {
    if (files.length !== 1) return { error: 'An Allure 3 import takes one history file.' };
    const [f] = files;
    if (!f) return { error: 'Choose the history file.' };
    if (f.size === 0) return { error: `${f.name} is empty.` };
    if (f.size > ALLURE3_MAX) return { error: `${f.name} is larger than 256 MiB.` };
    form.append('history', f, f.name);
    return { form };
  }
  const seen = new Set<string>();
  for (const f of files) {
    const spec = ALLURE2_FILES[f.name];
    if (!spec) return { error: `${f.name}: expected history.json or one of history-trend.json, duration-trend.json, categories-trend.json, retry-trend.json.` };
    if (seen.has(spec.field)) return { error: `${f.name} is chosen twice.` };
    if (f.size === 0) return { error: `${f.name} is empty.` };
    if (f.size > spec.max) return { error: `${f.name} is larger than ${String(spec.max / MIB)} MiB.` };
    seen.add(spec.field);
    form.append(spec.field, f, f.name);
  }
  if (!seen.has('history')) return { error: 'history.json is required.' };
  return { form };
}

export function useCreateHistoryImport(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (v: { source: HistorySource; branch?: string; environment?: string; form: FormData }) =>
      request<HistoryImport>(withQuery(`${projectPath(key)}/history-imports`, { source: v.source, branch: v.branch?.trim(), environment: v.environment?.trim() }), {
        method: 'POST',
        body: v.form,
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: historyKeys.list(key) });
    },
  });
}

/** Launches, results and flaky data change when an import ends. */
export function useRefreshAfterImport(key: string) {
  const qc = useQueryClient();
  return () => {
    for (const k of ['launches', 'launch', 'results', 'flaky', 'dashboard', 'project']) void qc.invalidateQueries({ queryKey: [k, key] });
  };
}
