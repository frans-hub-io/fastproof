import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useMemo } from 'react';
import { API, projectPath, request, withQuery } from './client';
import { qk } from './hooks';
import type { ItemList, Launch, ScheduledJob } from './types';
import type {
  Cluster,
  Dashboard,
  DashboardFilters,
  DefectType,
  DefectTypeInput,
  Delivery,
  FlakyTest,
  GateResult,
  LaunchComparison,
  LaunchPatch,
  MockInteractions,
  NotificationTestResult,
  ProjectSettings,
  Rerun,
  RerunList,
  RerunRequest,
  Rule,
  RuleInput,
  RuleTestResponse,
  SecretInfo,
  SecretPurpose,
  SimilarFailure,
  TestPlanCreate,
  TestPlanCreated,
  TriageRequest,
  TriageResponse,
} from './types-next';

const enc = encodeURIComponent;

export const qc2 = {
  defectTypes: (key: string) => ['defectTypes', key] as const,
  clusters: (key: string, launch: string) => ['clusters', key, launch] as const,
  similar: (key: string, result: string) => ['similar', key, result] as const,
  rules: (key: string) => ['rules', key] as const,
  settings: (key: string) => ['settings', key] as const,
  secrets: (key: string) => ['secrets', key] as const,
  flaky: (key: string, f?: { branch?: string; environment?: string }) => (f ? (['flaky', key, f] as const) : (['flaky', key] as const)),
  compare: (key: string, a: string, b: string) => ['compare', key, a, b] as const,
  dashboard: (key: string, f?: DashboardFilters) => (f ? (['dashboard', key, f] as const) : (['dashboard', key] as const)),
  gate: (key: string, launch: string) => ['gate', key, launch] as const,
  deliveries: (key: string) => ['deliveries', key] as const,
  mockInteractions: (key: string, result: string, attempt: number) => ['mockInteractions', key, result, attempt] as const,
  jobs: () => ['jobs'] as const,
};

export function useDefectTypes(key: string) {
  return useQuery({
    queryKey: qc2.defectTypes(key),
    queryFn: () => request<ItemList<DefectType>>(`${projectPath(key)}/defect-types`).then((r) => r.items),
    staleTime: 60_000,
  });
}

export function useDefectTypeMap(key: string): Map<string, DefectType> {
  const q = useDefectTypes(key);
  return useMemo(() => new Map((q.data ?? []).map((t) => [t.id, t] as const)), [q.data]);
}

export function useSaveDefectType(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id?: string; body: DefectTypeInput }) =>
      id
        ? request<DefectType>(`${projectPath(key)}/defect-types/${enc(id)}`, { method: 'PATCH', json: body })
        : request<DefectType>(`${projectPath(key)}/defect-types`, { method: 'POST', json: body }),
    onSuccess: () => qc.invalidateQueries({ queryKey: qc2.defectTypes(key) }),
  });
}

export function useDeleteDefectType(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => request<unknown>(`${projectPath(key)}/defect-types/${enc(id)}`, { method: 'DELETE' }),
    onSuccess: () => qc.invalidateQueries({ queryKey: qc2.defectTypes(key) }),
  });
}

export function useTriage(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: TriageRequest) => request<TriageResponse>(`${projectPath(key)}/triage`, { method: 'POST', json: { source: 'user', ...body } }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ['results', key] });
      void qc.invalidateQueries({ queryKey: ['result', key] });
      void qc.invalidateQueries({ queryKey: ['clusters', key] });
      void qc.invalidateQueries({ queryKey: ['similar', key] });
      void qc.invalidateQueries({ queryKey: ['launch', key] });
    },
  });
}

export function useClusters(key: string, launchId: string, enabled = true) {
  return useQuery({
    queryKey: qc2.clusters(key, launchId),
    queryFn: () => request<ItemList<Cluster>>(`${projectPath(key)}/launches/${enc(launchId)}/clusters`).then((r) => r.items),
    enabled,
  });
}

export function useSimilar(key: string, resultId: string, limit = 10) {
  return useQuery({
    queryKey: qc2.similar(key, resultId),
    queryFn: () => request<ItemList<SimilarFailure>>(withQuery(`${projectPath(key)}/results/${enc(resultId)}/similar`, { limit })).then((r) => r.items),
  });
}

export function useMockInteractions(key: string, resultId: string, attempt: number, enabled: boolean) {
  return useQuery({
    queryKey: qc2.mockInteractions(key, resultId, attempt),
    queryFn: () => request<MockInteractions>(withQuery(`${projectPath(key)}/results/${enc(resultId)}/mock-interactions`, { attempt })),
    enabled,
  });
}

export function useRules(key: string) {
  return useQuery({
    queryKey: qc2.rules(key),
    queryFn: () => request<ItemList<Rule>>(`${projectPath(key)}/rules`).then((r) => [...r.items].sort((a, b) => a.order - b.order)),
  });
}

export function useSaveRule(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id?: string; body: Partial<RuleInput> }) =>
      id ? request<Rule>(`${projectPath(key)}/rules/${enc(id)}`, { method: 'PATCH', json: body }) : request<Rule>(`${projectPath(key)}/rules`, { method: 'POST', json: body }),
    onSuccess: () => qc.invalidateQueries({ queryKey: qc2.rules(key) }),
  });
}

export function useDeleteRule(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => request<unknown>(`${projectPath(key)}/rules/${enc(id)}`, { method: 'DELETE' }),
    onSuccess: () => qc.invalidateQueries({ queryKey: qc2.rules(key) }),
  });
}

export function useTestRule(key: string) {
  return useMutation({
    mutationFn: (body: { rule: Partial<RuleInput>; launchId: string }) => request<RuleTestResponse>(`${projectPath(key)}/rules/test`, { method: 'POST', json: body }),
  });
}

export function useApplyRule(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, launchId }: { id: string; launchId: string }) =>
      request<{ updated?: number; matched?: number } | null>(withQuery(`${projectPath(key)}/rules/${enc(id)}/apply`, { launchId }), { method: 'POST' }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: qc2.rules(key) });
      void qc.invalidateQueries({ queryKey: ['results', key] });
      void qc.invalidateQueries({ queryKey: ['clusters', key] });
    },
  });
}

export function useSettings(key: string) {
  return useQuery({
    queryKey: qc2.settings(key),
    queryFn: () => request<ProjectSettings>(`${projectPath(key)}/settings`).then(withSettingsDefaults),
  });
}

export function withSettingsDefaults(s: Partial<ProjectSettings>): ProjectSettings {
  return {
    // Areas this UI does not edit (stage 2a `caseSync`, newer server keys) travel back unchanged on save.
    ...s,
    flaky: { historyDepth: 10, stabilizationPeriod: 5, includePassedTests: false, ...s.flaky },
    retention: { rawDays: 90, keepImportant: true, ...s.retention },
    rollup: { afterDays: 30, ...s.rollup },
    gates: s.gates ?? [],
    notifications: s.notifications ?? [],
    issueTrackers: s.issueTrackers ?? [],
    vcs: s.vcs ?? null,
    fastmock: s.fastmock ?? null,
    rerun: s.rerun ?? null,
  };
}

export function useSaveSettings(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: ProjectSettings) => request<ProjectSettings>(`${projectPath(key)}/settings`, { method: 'PUT', json: body }),
    onSuccess: (data) => {
      qc.setQueryData(qc2.settings(key), withSettingsDefaults(data));
      void qc.invalidateQueries({ queryKey: ['gate', key] });
    },
  });
}

export function useSecrets(key: string, enabled = true) {
  return useQuery({
    queryKey: qc2.secrets(key),
    queryFn: () => request<ItemList<SecretInfo>>(`${projectPath(key)}/secrets`).then((r) => r.items),
    enabled,
  });
}

export function useWriteSecret(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ name, value, purpose, origin }: { name: string; value: string; purpose: SecretPurpose; origin?: string | null }) =>
      request<unknown>(`${projectPath(key)}/secrets/${enc(name)}`, { method: 'PUT', json: { value, purpose, ...(origin ? { origin } : {}) } }),
    onSuccess: () => qc.invalidateQueries({ queryKey: qc2.secrets(key) }),
  });
}

export function useFlaky(key: string, f: { branch?: string; environment?: string; limit?: number }) {
  return useQuery({
    queryKey: qc2.flaky(key, f),
    queryFn: () => request<ItemList<FlakyTest>>(withQuery(`${projectPath(key)}/flaky`, { ...f })).then((r) => r.items),
  });
}

export function useCompare(key: string, a: string, b: string) {
  return useQuery({
    queryKey: qc2.compare(key, a, b),
    queryFn: () => request<LaunchComparison>(`${projectPath(key)}/launches/${enc(a)}/compare/${enc(b)}`),
    enabled: !!a && !!b && a !== b,
  });
}

export function useDashboard(key: string, f: DashboardFilters) {
  return useQuery({
    queryKey: qc2.dashboard(key, f),
    queryFn: () => request<Dashboard>(withQuery(`${projectPath(key)}/dashboard`, { ...f })),
  });
}

export function useGate(key: string, launch: Launch) {
  return useQuery({
    queryKey: qc2.gate(key, launch.id),
    queryFn: () => request<GateResult>(`${projectPath(key)}/launches/${enc(launch.id)}/gate`),
    refetchInterval: (q) => (q.state.data?.status === 'pending' || launch.status === 'running' ? 20_000 : false),
    retry: false,
  });
}

export function useDeliveries(key: string) {
  return useQuery({
    queryKey: qc2.deliveries(key),
    queryFn: () => request<ItemList<Delivery>>(withQuery(`${projectPath(key)}/notifications/deliveries`, { limit: 100 })).then((r) => r.items),
  });
}

/** Sends a sample message to one saved notification (`POST .../notifications/{id}/test`). */
export function useTestNotification(key: string) {
  return useMutation({
    mutationFn: (id: string) => request<NotificationTestResult>(`${projectPath(key)}/notifications/${enc(id)}/test`, { method: 'POST' }),
  });
}

export function usePatchLaunch(key: string, id: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: LaunchPatch) => request<Launch>(`${projectPath(key)}/launches/${enc(id)}`, { method: 'PATCH', json: body }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: qk.launch(key, id) });
      void qc.invalidateQueries({ queryKey: qk.launches(key) });
    },
  });
}

export function useCreateTestPlan(key: string) {
  return useMutation({
    mutationFn: (body: TestPlanCreate) => request<TestPlanCreated>(`${projectPath(key)}/test-plans`, { method: 'POST', json: body }),
  });
}

export function useReruns(key: string, launchId: string) {
  return useQuery({
    queryKey: ['reruns', key, launchId],
    queryFn: () => request<RerunList>(`${projectPath(key)}/launches/${enc(launchId)}/reruns`),
  });
}

export function useRerun(key: string, launchId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: RerunRequest) => request<Rerun>(`${projectPath(key)}/launches/${enc(launchId)}/rerun`, { method: 'POST', json: body }),
    onSettled: () => void qc.invalidateQueries({ queryKey: ['reruns', key, launchId] }),
  });
}

export function testPlanUrl(key: string, id: string): string {
  return `${projectPath(key)}/test-plans/${enc(id)}/testplan.json`;
}

export function useJobs(enabled = true) {
  return useQuery({
    queryKey: qc2.jobs(),
    queryFn: () => request<ItemList<ScheduledJob>>(`${API}/admin/jobs`).then((r) => r.items),
    refetchInterval: 30_000,
    enabled,
  });
}

