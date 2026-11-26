// Failure analytics (FastProof Analysis): trends, top errors, defect groups over time, MTTR,
// retry cost, slow steps, duration regressions and the portfolio. ClickHouse answers up to a
// year; without it PostgreSQL answers the last 30 days. The DTOs are the generated
// `schema.gen.ts` types.
import { keepPreviousData, useQuery } from '@tanstack/react-query';
import { ApiError, API, projectPath, request, withQuery } from './client';
import type { components } from './schema.gen';

type S = components['schemas'];

export type AnalyticsWindow = S['AnalyticsWindow'];
export type FailureGroupBy = S['FailureGroupBy'];
export type FailureGroup = S['FailureGroup'];
export type FailureAnalytics = S['FailureAnalytics'];
export type DurationRegressions = S['DurationRegressions'];
export type DurationRegression = S['DurationRegression'];
export type RetryCosts = S['RetryCosts'];
export type FlakyTrend = S['FlakyTrend'];
export type MttrStats = S['MttrStats'];
export type SlowSteps = S['SlowSteps'];
export type Portfolio = S['Portfolio'];
export type PortfolioProject = S['PortfolioProject'];
export type CrossProjectSimilar = S['CrossProjectSimilar'];

export const GROUP_BY_LABEL: Record<FailureGroupBy, string> = {
  fingerprint: 'Unique error',
  defectGroup: 'Defect group',
  suite: 'Suite',
  env: 'Environment',
};

/** Window lengths the page offers, in days. */
export const WINDOW_DAYS = [7, 30, 90, 365] as const;

export interface AnalyticsFilter {
  days: number;
  branch?: string;
}

function range(f: AnalyticsFilter, now: number): { from: string; to: string; branch?: string } {
  const to = Math.ceil(now / 60_000) * 60_000;
  return { from: new Date(to - f.days * 86_400_000).toISOString(), to: new Date(to).toISOString(), branch: f.branch || undefined };
}

export const analyticsKeys = {
  all: (key: string) => ['analytics', key] as const,
  one: (key: string, what: string, f: AnalyticsFilter, extra?: string) => ['analytics', key, what, f, extra ?? ''] as const,
  portfolio: (days: number) => ['analytics', 'portfolio', days] as const,
};

function useProjectAnalytics<T>(key: string, what: string, f: AnalyticsFilter, extra: Record<string, string | number> = {}) {
  return useQuery({
    queryKey: analyticsKeys.one(key, what, f, JSON.stringify(extra)),
    queryFn: () => request<T>(withQuery(`${projectPath(key)}/analytics/${what}`, { ...range(f, Date.now()), ...extra })),
    enabled: !!key,
    placeholderData: keepPreviousData,
    staleTime: 60_000,
  });
}

/** Failures grouped by unique error, defect group, suite or environment. */
export function useFailureAnalytics(key: string, f: AnalyticsFilter, groupBy: FailureGroupBy, limit = 10) {
  return useProjectAnalytics<FailureAnalytics>(key, 'failures', f, { groupBy, limit });
}

export function useDurationRegressions(key: string, f: AnalyticsFilter) {
  return useProjectAnalytics<DurationRegressions>(key, 'durations', f, { limit: 10 });
}

export function useRetryCosts(key: string, f: AnalyticsFilter) {
  return useProjectAnalytics<RetryCosts>(key, 'retries', f, { limit: 10 });
}

export function useFlakyTrend(key: string, f: AnalyticsFilter) {
  return useProjectAnalytics<FlakyTrend>(key, 'flaky-trend', f);
}

/** MTTR reads the error catalog: no branch filter. */
export function useMttr(key: string, f: AnalyticsFilter) {
  return useProjectAnalytics<MttrStats>(key, 'mttr', { days: f.days });
}

/** Steps carry no branch in ClickHouse: no branch filter. */
export function useSlowSteps(key: string, f: AnalyticsFilter) {
  return useProjectAnalytics<SlowSteps>(key, 'slow-steps', { days: f.days }, { limit: 10 });
}

/** The portfolio of every project the caller can read; `null` when the server has no ClickHouse. */
export function usePortfolio(days: number, enabled = true) {
  return useQuery({
    queryKey: analyticsKeys.portfolio(days),
    queryFn: async (): Promise<Portfolio | null> => {
      const to = Math.ceil(Date.now() / 60_000) * 60_000;
      try {
        return await request<Portfolio>(withQuery(`${API}/analytics/portfolio`, { from: new Date(to - days * 86_400_000).toISOString(), to: new Date(to).toISOString() }));
      } catch (e) {
        if (isClickHouseRequired(e)) return null;
        throw e;
      }
    },
    enabled,
    staleTime: 60_000,
  });
}

export function isClickHouseRequired(e: unknown): boolean {
  return e instanceof ApiError && e.status === 409 && e.code === 'clickhouse_required';
}

/** Human text of a window: "ClickHouse" or "PostgreSQL, last 30 days" and "partial". */
export function windowNote(w: AnalyticsWindow | undefined): string {
  if (!w) return '';
  const base = w.backend === 'clickhouse' ? 'ClickHouse' : 'PostgreSQL, last 30 days';
  return w.partial ? `${base} (partial)` : base;
}

/** Seconds as the largest whole unit: "45 s", "3 h", "2.5 d". */
export function formatSeconds(s: number | null | undefined): string {
  if (s === null || s === undefined || !Number.isFinite(s)) return '—';
  if (s < 60) return `${String(Math.round(s))} s`;
  if (s < 3600) return `${String(Math.round(s / 60))} min`;
  if (s < 86_400) return `${(s / 3600).toFixed(s < 36_000 ? 1 : 0)} h`;
  return `${(s / 86_400).toFixed(s < 864_000 ? 1 : 0)} d`;
}
