import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { ApiError, projectPath, request } from './client';
import type { components } from './schema.gen';

type S = components['schemas'];

export type QuerySchema = S['QuerySchema'];
export type QueryAttribute = S['QueryAttribute'];
export type QueryError = S['QueryError'];
export type QueryValidation = S['QueryValidation'];
export type SavedQuery = S['SavedQuery'];
export type SavedQueryWrite = S['SavedQueryWrite'];
export type SavedQueryPatch = S['SavedQueryPatch'];

const qk = {
  schema: (key: string) => ['queries', key, 'schema'] as const,
  validate: (key: string, query: string) => ['queries', key, 'validate', query] as const,
  saved: (key: string) => ['queries', key, 'saved'] as const,
};

export function useQuerySchema(key: string) {
  return useQuery({
    queryKey: qk.schema(key),
    queryFn: () => request<QuerySchema>(`${projectPath(key)}/query/schema?entity=cases`),
    staleTime: 5 * 60_000,
    retry: false,
  });
}

export function validateQuery(key: string, query: string): Promise<QueryValidation> {
  return request<QueryValidation>(`${projectPath(key)}/query/validate`, { method: 'POST', json: { entity: 'cases', query } });
}

/** Validation of a query text; the caller debounces the text. Empty text is not sent. */
export function useValidateQuery(key: string, query: string) {
  // Sent as typed: error positions count from the start of the text in the editor.
  return useQuery({
    queryKey: qk.validate(key, query),
    queryFn: () => validateQuery(key, query),
    enabled: query.trim().length > 0,
    placeholderData: keepPreviousData,
    staleTime: 30_000,
    retry: false,
  });
}

/** The `QueryError` of a 400 `invalid_query` response, if the error is one. */
export function invalidQueryDetails(err: unknown): QueryError | null {
  if (!(err instanceof ApiError) || err.code !== 'invalid_query') return null;
  const d = err.details as Partial<QueryError> | undefined;
  if (!d || typeof d.position !== 'number') return null;
  return { position: d.position, length: d.length ?? 1, expected: d.expected ?? [], near: d.near ?? '', message: d.message ?? err.message };
}

export function useSavedQueries(key: string) {
  return useQuery({
    queryKey: qk.saved(key),
    queryFn: () => request<S['SavedQueryList']>(`${projectPath(key)}/saved-queries?entity=cases`).then((r) => r.items),
  });
}

export function useCreateSavedQuery(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: SavedQueryWrite) => request<SavedQuery>(`${projectPath(key)}/saved-queries`, { method: 'POST', json: body }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: qk.saved(key) }),
  });
}

export function useUpdateSavedQuery(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, patch }: { id: string; patch: SavedQueryPatch }) => request<SavedQuery>(`${projectPath(key)}/saved-queries/${encodeURIComponent(id)}`, { method: 'PATCH', json: patch }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: qk.saved(key) }),
  });
}

export function useDeleteSavedQuery(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => request<unknown>(`${projectPath(key)}/saved-queries/${encodeURIComponent(id)}`, { method: 'DELETE' }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: qk.saved(key) }),
  });
}
