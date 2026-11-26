import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { projectPath, request } from './client';
import type { components } from './schema.gen';
import { tmk, writeVersioned, type RunItem, type Versioned } from './tm';

type S = components['schemas'];

export type CaseStep = S['CaseStep'];
export type ParameterMode = S['ParameterMode'];
export type ParameterRow = S['ParameterRow'];
export type ParameterGenerator = S['ParameterGenerator'];
export type CaseParameters = S['CaseParameters'];
export type ParameterGenerate = S['ParameterGenerate'];
export type ParameterRows = S['ParameterRows'];
export type RunIteration = S['RunIteration'];
export type RunIterationPatch = S['RunIterationPatch'];
export type RunIterationStepAck = S['RunIterationStepAck'];
export type RunStepWrite = S['RunStepWrite'];

/** `runIteration.updated` (SSE). */
export interface RunIterationUpdated {
  launchId: string;
  itemId: string;
  rowId: string;
  status: S['RunStatus'];
  revision: number;
}

const enc = encodeURIComponent;

/** Rows from a generator input (nothing is saved). */
export function generateParameters(key: string, n: number, body: ParameterGenerate): Promise<ParameterRows> {
  return request<ParameterRows>(`${projectPath(key)}/cases/${String(n)}/parameters/generate`, { method: 'POST', json: body });
}

export function useGenerateParameters(key: string, n: number) {
  return useMutation({ mutationFn: (body: ParameterGenerate) => generateParameters(key, n, body) });
}

/** Writes one step of an iteration (no `If-Match`: the last writer wins). */
export function putIterationStep(key: string, itemId: string, rowId: string, path: string, body: RunStepWrite): Promise<RunIterationStepAck> {
  return request<RunIterationStepAck>(`${projectPath(key)}/run-items/${enc(itemId)}/iterations/${enc(rowId)}/steps/${enc(path)}`, { method: 'PUT', json: body });
}

/** Changes the status or comment of an iteration (`If-Match` of the item). */
export function patchRunIteration(key: string, itemId: string, rowId: string, etag: string, patch: RunIterationPatch): Promise<Versioned<RunItem>> {
  return writeVersioned<RunItem>(`${projectPath(key)}/run-items/${enc(itemId)}/iterations/${enc(rowId)}`, 'PATCH', etag, patch);
}

export function usePatchRunIteration(key: string, itemId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ rowId, etag, patch }: { rowId: string; etag: string; patch: RunIterationPatch }) => patchRunIteration(key, itemId, rowId, etag, patch),
    onSuccess: (v) => {
      qc.setQueryData(tmk.runItem(key, itemId), v);
      void qc.invalidateQueries({ queryKey: tmk.runItems(key, v.value.launchId) });
      void qc.invalidateQueries({ queryKey: tmk.launch(key, v.value.launchId) });
    },
  });
}

/** One iteration with all its step results (`GET /run-items/{id}` leaves them out of later iterations of a big item). */
export function getRunIteration(key: string, itemId: string, rowId: string, executionNo?: number): Promise<RunIteration> {
  const q = executionNo === undefined ? '' : `?executionNo=${String(executionNo)}`;
  return request<RunIteration>(`${projectPath(key)}/run-items/${enc(itemId)}/iterations/${enc(rowId)}${q}`);
}

/** The full iteration, read again when the item's revision changes; `enabled` only while the item's copy omits its steps. */
export function useRunIteration(key: string, itemId: string, rowId: string, executionNo: number, revision: number, enabled: boolean) {
  return useQuery({
    queryKey: ['tm', 'run-iteration', key, itemId, rowId, executionNo, revision],
    queryFn: () => getRunIteration(key, itemId, rowId, executionNo),
    enabled,
    placeholderData: (prev, prevQuery) => (prevQuery?.queryKey[4] === rowId ? prev : undefined),
  });
}
