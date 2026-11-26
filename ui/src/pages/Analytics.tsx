import { Alert, Group, SegmentedControl, SimpleGrid, Stack, Tabs, Text, TextInput } from '@mantine/core';
import { useDebouncedValue } from '@mantine/hooks';
import { IconDatabase } from '@tabler/icons-react';
import { useEffect, useState } from 'react';
import { useParams, useSearchParams } from 'react-router';
import { useAnalysisCapabilities } from '../api/analysis';
import {
  useDurationRegressions,
  useFailureAnalytics,
  useFlakyTrend,
  useMttr,
  usePortfolio,
  useRetryCosts,
  useSlowSteps,
  WINDOW_DAYS,
  type AnalyticsFilter,
  type FailureGroupBy,
} from '../api/analytics';
import { useProject } from '../api/hooks';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { BackendBanner, DefectGroupsChart, DurationsCard, FlakyTrendChart, MttrCard, PortfolioTable, RetriesCard, SlowStepsCard, TopFailures } from './analytics/parts';

const GROUP_BYS: FailureGroupBy[] = ['fingerprint', 'defectGroup', 'suite', 'env'];

function daysLabel(d: number): string {
  return d === 365 ? '1 year' : `${String(d)} days`;
}

function ProjectView({ projectKey, f }: { projectKey: string; f: AnalyticsFilter }) {
  const [params, setParams] = useSearchParams();
  const g = params.get('groupBy');
  const groupBy: FailureGroupBy = GROUP_BYS.includes(g as FailureGroupBy) ? (g as FailureGroupBy) : 'fingerprint';
  const setGroupBy = (v: FailureGroupBy) =>
    setParams(
      (p) => {
        if (v === 'fingerprint') p.delete('groupBy');
        else p.set('groupBy', v);
        return p;
      },
      { replace: true },
    );
  const top = useFailureAnalytics(projectKey, f, groupBy);
  const defects = useFailureAnalytics(projectKey, f, 'defectGroup');
  const flaky = useFlakyTrend(projectKey, f);
  const mttr = useMttr(projectKey, f);
  const retries = useRetryCosts(projectKey, f);
  const steps = useSlowSteps(projectKey, f);
  const durations = useDurationRegressions(projectKey, f);
  return (
    <Stack gap="md">
      <BackendBanner w={top.data?.window} />
      <QueryState query={top}>{() => top.data && <TopFailures data={top.data} projectKey={projectKey} groupBy={groupBy} onGroupBy={setGroupBy} />}</QueryState>
      <SimpleGrid cols={{ base: 1, md: 2 }} spacing="md">
        <QueryState query={defects}>{() => defects.data && <DefectGroupsChart data={defects.data} />}</QueryState>
        <QueryState query={flaky}>{() => flaky.data && <FlakyTrendChart data={flaky.data} />}</QueryState>
        <QueryState query={mttr}>{() => mttr.data && <MttrCard data={mttr.data} />}</QueryState>
        <QueryState query={retries}>{() => retries.data && <RetriesCard data={retries.data} />}</QueryState>
        <QueryState query={steps}>{() => steps.data && <SlowStepsCard data={steps.data} />}</QueryState>
        <QueryState query={durations}>{() => durations.data && <DurationsCard data={durations.data} projectKey={projectKey} />}</QueryState>
      </SimpleGrid>
    </Stack>
  );
}

function PortfolioView({ days }: { days: number }) {
  const q = usePortfolio(days);
  return (
    <QueryState query={q}>
      {() =>
        q.data === null ? (
          <Alert variant="light" color="blue" icon={<IconDatabase size={16} />} data-testid="analytics-portfolio-needs-clickhouse">
            The portfolio compares every project you can read and needs ClickHouse. Ask an administrator to turn on the ClickHouse sink for Analysis.
          </Alert>
        ) : (
          q.data && <PortfolioTable data={q.data} />
        )
      }
    </QueryState>
  );
}

/** Analytics (FastProof Analysis): failure trends, top errors, defect groups over time, MTTR,
 * retry cost, slow steps, slower tests, and the portfolio of the projects a person can read. */
export default function AnalyticsPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const caps = useAnalysisCapabilities(key);
  const [params, setParams] = useSearchParams();
  const d = Number(params.get('days'));
  const days = (WINDOW_DAYS as readonly number[]).includes(d) ? d : 30;
  const tab = params.get('tab') === 'portfolio' ? 'portfolio' : 'project';
  const [branch, setBranch] = useState(params.get('branch') ?? '');
  const [debounced] = useDebouncedValue(branch.trim(), 400);
  useEffect(() => {
    setParams(
      (p) => {
        if (debounced) p.set('branch', debounced);
        else p.delete('branch');
        return p;
      },
      { replace: true },
    );
  }, [debounced, setParams]);
  const set = (k: string, v: string | null) =>
    setParams(
      (p) => {
        if (v === null) p.delete(k);
        else p.set(k, v);
        return p;
      },
      { replace: true },
    );
  const f: AnalyticsFilter = { days, branch: debounced || undefined };
  const longOnPg = caps.data && !caps.data.clickhouse && days > 30;
  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Analytics' }]}
        title="Analytics"
        subtitle="Failure trends, slow tests and retry costs"
      />
      <Group gap="sm" mb="md" wrap="wrap" align="flex-end">
        <SegmentedControl
          value={String(days)}
          onChange={(v) => set('days', v === '30' ? null : v)}
          data={WINDOW_DAYS.map((x) => ({ value: String(x), label: daysLabel(x) }))}
          data-testid="analytics-days"
        />
        {tab === 'project' && <TextInput placeholder="All branches" aria-label="Branch" value={branch} onChange={(e) => setBranch(e.currentTarget.value)} w={{ base: '100%', xs: 220 }} data-testid="analytics-branch" />}
      </Group>
      {longOnPg && tab === 'project' && (
        <Text size="xs" c="dimmed" mb="xs">
          Without ClickHouse the answers cover the last 30 days.
        </Text>
      )}
      <Tabs value={tab} onChange={(v) => set('tab', v === 'portfolio' ? 'portfolio' : null)} keepMounted={false}>
        <Tabs.List mb="md">
          <Tabs.Tab value="project" data-testid="analytics-tab-project">
            This project
          </Tabs.Tab>
          <Tabs.Tab value="portfolio" data-testid="analytics-tab-portfolio">
            Portfolio
          </Tabs.Tab>
        </Tabs.List>
        <Tabs.Panel value="project">
          <ProjectView projectKey={key} f={f} />
        </Tabs.Panel>
        <Tabs.Panel value="portfolio">
          <PortfolioView days={days} />
        </Tabs.Panel>
      </Tabs>
    </>
  );
}
