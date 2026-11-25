// CI from a plan: dispatches of a plan's launches and the plan check of a finished launch.
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect } from 'react';
import { ApiError, projectPath, request, withQuery } from './client';
import { eventStreamSupported, openEventStream } from './sse';
import type { components } from './schema.gen';

type S = components['schemas'];

export type PlanCi = S['PlanCi'];
export type PlanCiEnvironment = S['PlanCiEnvironment'];
export type PlanDispatch = S['PlanDispatch'];
export type PlanDispatchList = S['PlanDispatchList'];
export type PlanCheck = S['PlanCheck'];
export type RerunVariable = S['RerunVariable'];

const enc = encodeURIComponent;

export const pdk = {
  dispatches: (key: string, planId: string) => ['tm', key, 'plan', planId, 'dispatches'] as const,
  planCheck: (key: string, launchId: string) => ['tm', key, 'launch', launchId, 'planCheck'] as const,
};

/** The dispatches of a plan, newest first; polls while one is still being sent and follows `planDispatch.updated`. */
export function usePlanDispatches(key: string, planId: string) {
  const qc = useQueryClient();
  useEffect(() => {
    if (!key || !planId || !eventStreamSupported()) return;
    const es = openEventStream(`${projectPath(key)}/events`, {
      events: {
        'planDispatch.updated': () => {
          void qc.invalidateQueries({ queryKey: pdk.dispatches(key, planId) });
        },
      },
    });
    return () => es.close();
  }, [key, planId, qc]);
  return useQuery({
    queryKey: pdk.dispatches(key, planId),
    queryFn: () => request<PlanDispatchList>(withQuery(`${projectPath(key)}/test-plans/${enc(planId)}/dispatches`, { limit: 20 })),
    enabled: !!key && !!planId,
    refetchInterval: (q) => (q.state.data?.items.some((d) => d.status === 'dispatching') ? 3000 : false),
  });
}

/** What a finished launch of a plan ran compared to the plan; `null` while there is nothing to show (no plan, not finished). */
export function useLaunchPlanCheck(key: string, launchId: string, enabled = true) {
  return useQuery({
    queryKey: pdk.planCheck(key, launchId),
    queryFn: async () => {
      try {
        return await request<PlanCheck>(`${projectPath(key)}/launches/${enc(launchId)}/plan-check`);
      } catch (e) {
        if (e instanceof ApiError && (e.status === 404 || e.status === 403)) return null;
        throw e;
      }
    },
    enabled: !!key && !!launchId && enabled,
    staleTime: 30_000,
    retry: false,
  });
}

/** A `NAME=VALUE` form of the variables the CI was given, in name order. */
export function variablesLine(v: Record<string, string>): string {
  return Object.entries(v)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([k, val]) => `${k}=${val}`)
    .join(' ');
}
