import { keepPreviousData, useInfiniteQuery, useMutation, useQuery, useQueryClient, type InfiniteData } from '@tanstack/react-query';
import { API, projectPath, request, withQuery } from './client';
import { limiter, withBusyRetry } from './retry';
import type {
  AuditEvent,
  AuditFilters,
  AuditPage,
  CreatedToken,
  CreateToken,
  ExplorerParams,
  ItemList,
  LabelName,
  Launch,
  EnvironmentCounts,
  LaunchFacets,
  LaunchFilters,
  Page,
  Project,
  ResultDetail,
  ResultFilters,
  ResultSummary,
  TestCaseHistoryFilters,
  TestCaseHistoryItem,
  TokenList,
  TokenOptions,
  TreeFilters,
  TreeGroupBy,
  TreeNode,
  TreeTest,
  Upload,
  VersionInfo,
} from './types';

export const qk = {
  projects: () => ['projects'] as const,
  project: (key: string) => ['project', key] as const,
  launches: (key: string, filters?: LaunchFilters) => (filters ? (['launches', key, filters] as const) : (['launches', key] as const)),
  launch: (key: string, id: string) => ['launch', key, id] as const,
  uploads: (key: string, id: string) => ['uploads', key, id] as const,
  results: (key: string, id: string, filters?: ResultFilters) => (filters ? (['results', key, id, filters] as const) : (['results', key, id] as const)),
  tree: (key: string, id: string, groupBy?: TreeGroupBy) => (groupBy ? (['tree', key, id, groupBy] as const) : (['tree', key, id] as const)),
  result: (key: string, id: string) => ['result', key, id] as const,
  testCase: (key: string, id: string, filters?: TestCaseHistoryFilters) => (filters ? (['testCase', key, id, filters] as const) : (['testCase', key, id] as const)),
  tokens: () => ['tokens'] as const,
  tokenOptions: () => ['tokenOptions'] as const,
  audit: (filters?: AuditFilters) => (filters ? (['audit', filters] as const) : (['audit'] as const)),
  version: () => ['version'] as const,
};

export const LAUNCH_PAGE = 30;
export const RESULT_PAGE = 500;

/** The server's version, for the footer; it changes only with a new deployment. */
export function useServerVersion() {
  return useQuery({
    queryKey: qk.version(),
    queryFn: () => request<VersionInfo>(`${API}/version`).then((r) => r.version),
    staleTime: Infinity,
    retry: false,
  });
}

export function useProjects() {
  return useQuery({
    queryKey: qk.projects(),
    queryFn: () => request<ItemList<Project>>(`${API}/projects`).then((r) => r.items),
  });
}

export function useProject(key: string) {
  return useQuery({
    queryKey: qk.project(key),
    queryFn: () => request<Project>(projectPath(key)),
  });
}

export function useLatestLaunch(key: string, enabled: boolean) {
  return useQuery({
    queryKey: [...qk.launches(key), 'latest'],
    queryFn: () => request<Page<Launch>>(withQuery(`${projectPath(key)}/launches`, { limit: 1 })).then((r) => r.items[0] ?? null),
    enabled,
  });
}

export function launchQuery(filters: LaunchFilters) {
  return {
    branch: filters.branch,
    environment: filters.environment,
    status: filters.status,
    attr: filters.attr,
    q: filters.q,
    debug: filters.debug === undefined ? undefined : String(filters.debug),
  };
}

export function useLaunches(key: string, filters: LaunchFilters) {
  return useInfiniteQuery({
    queryKey: qk.launches(key, filters),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      request<Page<Launch>>(withQuery(`${projectPath(key)}/launches`, { ...launchQuery(filters), before: pageParam, limit: LAUNCH_PAGE })),
    getNextPageParam: (last) => last.next ?? undefined,
  });
}

export function flatItems<T>(data: InfiniteData<Page<T>> | undefined): T[] {
  return data ? data.pages.flatMap((p) => p.items) : [];
}

export function useLaunch(key: string, id: string) {
  return useQuery({
    queryKey: qk.launch(key, id),
    queryFn: () => request<Launch>(`${projectPath(key)}/launches/${encodeURIComponent(id)}`),
    enabled: !!id,
    refetchInterval: (q) => (q.state.data?.status === 'running' ? 15_000 : false),
  });
}

export function useUploads(key: string, id: string) {
  return useQuery({
    queryKey: qk.uploads(key, id),
    queryFn: () => request<ItemList<Upload>>(`${projectPath(key)}/launches/${encodeURIComponent(id)}/uploads`).then((r) => r.items),
  });
}

/** The query parameters of the explorer filters. */
export function explorerQuery(f: ExplorerParams) {
  return {
    message: f.message,
    retried: f.retried ? 'true' : undefined,
    muted: f.muted === undefined ? undefined : String(f.muted),
    newFailures: f.newFailures ? 'true' : undefined,
    severity: f.severity,
    owner: f.owner,
    tag: f.tag,
    minDurationMs: f.minDurationMs,
    maxDurationMs: f.maxDurationMs,
    defect: f.defect,
    attachment: f.attachment,
    env: f.env,
  };
}

export function resultQuery(filters: ResultFilters) {
  return {
    ...explorerQuery(filters),
    status: filters.status,
    q: filters.q,
    suite: filters.suite,
    label: filters.label,
    flaky: filters.flaky === undefined ? undefined : String(filters.flaky),
    fingerprint: filters.fingerprint,
    source: filters.source,
  };
}

export function useResults(key: string, id: string, filters: ResultFilters) {
  return useInfiniteQuery({
    queryKey: qk.results(key, id, filters),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      request<Page<ResultSummary>>(
        withQuery(`${projectPath(key)}/launches/${encodeURIComponent(id)}/results`, { ...resultQuery(filters), before: pageParam, limit: RESULT_PAGE }),
      ),
    getNextPageParam: (last) => last.next ?? undefined,
    enabled: !!id,
  });
}

export function useTree(key: string, id: string, groupBy: TreeGroupBy) {
  return useQuery({
    queryKey: qk.tree(key, id, groupBy),
    queryFn: () => request<TreeNode | TreeNode[] | ItemList<TreeNode>>(withQuery(`${projectPath(key)}/launches/${encodeURIComponent(id)}/tree`, { groupBy })).then(treeRoots),
  });
}

function treeFilterQuery(f: TreeFilters) {
  return { ...explorerQuery(f), groupBy: f.groupBy, status: f.status, q: f.q, flaky: f.flaky ? 'true' : undefined, source: f.source };
}

/** The label names of a launch (choices of a custom grouping). */
export function useLaunchLabels(key: string, id: string, env: string | undefined, enabled = true) {
  return useQuery({
    queryKey: [...qk.tree(key, id), 'labels', env ?? ''] as const,
    queryFn: () => request<ItemList<LabelName>>(withQuery(`${projectPath(key)}/launches/${encodeURIComponent(id)}/labels`, { env })).then((r) => r.items),
    enabled: !!id && enabled,
    staleTime: 60_000,
  });
}

/** Facets are refreshed this often while the launch runs and the filter menu is open. */
export const FACETS_POLL_MS = 30_000;
/** Environments are refreshed this often while the launch runs. */
export const ENVIRONMENTS_POLL_MS = 60_000;

/** How many results each explorer filter value keeps; polled only while `menuOpen`. */
export function useLaunchFacets(key: string, id: string, env: string | undefined, running: boolean, enabled = true, menuOpen = false) {
  return useQuery({
    queryKey: [...qk.tree(key, id), 'facets', env ?? ''] as const,
    queryFn: () => request<LaunchFacets>(withQuery(`${projectPath(key)}/launches/${encodeURIComponent(id)}/facets`, { env })),
    enabled: !!id && enabled,
    placeholderData: keepPreviousData,
    refetchInterval: running && menuOpen ? FACETS_POLL_MS : false,
    refetchIntervalInBackground: false,
  });
}

/** The environments the results of a launch ran in, with counts. */
export function useLaunchEnvironments(key: string, id: string, running: boolean) {
  return useQuery({
    queryKey: [...qk.tree(key, id), 'environments'] as const,
    queryFn: () => request<ItemList<EnvironmentCounts>>(`${projectPath(key)}/launches/${encodeURIComponent(id)}/environments`).then((r) => r.items),
    enabled: !!id,
    refetchInterval: running ? ENVIRONMENTS_POLL_MS : false,
    refetchIntervalInBackground: false,
  });
}

/** The launch tree with its tests as leaves (groups that do not fit are read with `fetchTreeTests`). */
export function useExplorerTree(key: string, id: string, f: TreeFilters, running: boolean, enabled = true) {
  return useQuery({
    queryKey: [...qk.tree(key, id), 'tests', f] as const,
    queryFn: () =>
      request<TreeNode>(withQuery(`${projectPath(key)}/launches/${encodeURIComponent(id)}/tree`, { ...treeFilterQuery(f), includeTests: 'true', reveal: f.reveal })),
    placeholderData: keepPreviousData,
    refetchInterval: running ? 15_000 : false,
    enabled: !!id && enabled,
  });
}

export const TREE_TESTS_PAGE = 500;

/** A page of the tests directly in one group of the launch tree. */
export function fetchTreeTests(key: string, id: string, f: TreeFilters, path: readonly string[], before: string | null): Promise<Page<TreeTest>> {
  return treeTestsSlot(() =>
    withBusyRetry(() =>
      request<Page<TreeTest>>(
        withQuery(`${projectPath(key)}/launches/${encodeURIComponent(id)}/tree/tests`, { ...treeFilterQuery(f), path: JSON.stringify(path), before: before ?? undefined, limit: TREE_TESTS_PAGE }),
      ),
    ),
  );
}

/** Group pages load two at a time, so the groups opened with the page do not use up the caller's heavy slots. */
const treeTestsSlot = limiter(2);

export function treeRoots(body: TreeNode | TreeNode[] | ItemList<TreeNode>): TreeNode[] {
  if (Array.isArray(body)) return body;
  if ('items' in body) return body.items;
  return body.children;
}

export function useResult(key: string, id: string) {
  return useQuery({
    queryKey: qk.result(key, id),
    queryFn: () => request<ResultDetail>(`${projectPath(key)}/results/${encodeURIComponent(id)}`),
    enabled: !!id,
  });
}

export function useTestCaseHistory(key: string, testCaseId: string, filters: TestCaseHistoryFilters) {
  return useQuery({
    queryKey: qk.testCase(key, testCaseId, filters),
    queryFn: () =>
      request<ItemList<TestCaseHistoryItem>>(withQuery(`${projectPath(key)}/test-cases/${encodeURIComponent(testCaseId)}/history`, { ...filters })).then(
        (r) => r.items,
      ),
  });
}

export function useTokens() {
  return useQuery({
    queryKey: qk.tokens(),
    queryFn: () => request<TokenList>(`${API}/tokens`).then((r) => r.tokens),
  });
}

export function useTokenOptions(enabled = true) {
  return useQuery({
    queryKey: qk.tokenOptions(),
    queryFn: () => request<TokenOptions>(`${API}/tokens/options`),
    enabled,
  });
}

export function useCreateToken() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateToken) => request<CreatedToken>(`${API}/tokens`, { method: 'POST', json: body }),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.tokens() }),
  });
}

export function useRevokeToken() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => request<unknown>(`${API}/tokens/${encodeURIComponent(id)}`, { method: 'DELETE' }),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.tokens() }),
  });
}

export interface AuditChunk {
  items: AuditEvent[];
  next: string | null;
}

export function auditChunk(page: AuditPage): AuditChunk {
  return { items: page.items, next: page.next ?? null };
}

export function useAudit(filters: AuditFilters, enabled: boolean) {
  return useInfiniteQuery({
    queryKey: qk.audit(filters),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => request<AuditPage>(withQuery(`${API}/audit`, { ...filters, before: pageParam, limit: 100 })).then(auditChunk),
    getNextPageParam: (last) => last.next ?? undefined,
    enabled,
  });
}
