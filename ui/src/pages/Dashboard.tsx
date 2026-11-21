import { BarChart, LineChart } from '@mantine/charts';
import { Anchor, Autocomplete, Card, Group, Progress, SegmentedControl, SimpleGrid, Stack, Text, TextInput, Tooltip } from '@mantine/core';
import { EmptyState } from '@franshub/ui-shell';
import { useMemo, useState, type ReactNode } from 'react';
import { Link, useParams, useSearchParams } from 'react-router';
import { useProject } from '../api/hooks';
import { useDashboard } from '../api/stageC';
import type { Dashboard, DashboardBucket, DefectGroup } from '../api/types-next';
import { DEFECT_GROUPS } from '../api/types-next';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { GROUP_COLOR, GROUP_LABEL } from '../lib/defects';
import { formatDuration } from '../lib/format';
import { useLaunchList } from '../lib/launches';

const DAY = 86_400_000;

function isoDay(t: number): string {
  return new Date(t).toISOString().slice(0, 10);
}

function defaultRange(): { from: string; to: string } {
  const now = Date.now();
  return { from: isoDay(now - 30 * DAY), to: isoDay(now) };
}

function shortDay(t: string): string {
  const d = new Date(`${t}T00:00:00Z`);
  return Number.isNaN(d.getTime()) ? t : d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', timeZone: 'UTC' });
}

function Panel({ title, hint, children, testId }: { title: string; hint?: string; children: ReactNode; testId?: string }) {
  return (
    <Card withBorder radius="md" padding="md" data-testid={testId} style={{ minWidth: 0 }}>
      <Stack gap={2} mb="sm">
        <Text fw={600} size="sm">
          {title}
        </Text>
        {hint && (
          <Text size="xs" c="dimmed">
            {hint}
          </Text>
        )}
      </Stack>
      {children}
    </Card>
  );
}

function Stat({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <Card withBorder radius="md" padding="sm">
      <Text size="xs" c="dimmed">
        {label}
      </Text>
      <Text fw={800} size="xl">
        {value}
      </Text>
      {hint && (
        <Text size="xs" c="dimmed">
          {hint}
        </Text>
      )}
    </Card>
  );
}

function hasData(d: Dashboard): boolean {
  return d.launches.some((l) => l.count > 0);
}

function Content({ project, d }: { project: string; d: Dashboard }) {
  const passRate = useMemo(() => d.passRate.map((p) => ({ t: shortDay(p.t), passRate: p.total > 0 ? Math.round(p.passRate * 1000) / 10 : null, total: p.total })), [d.passRate]);
  const durations = useMemo(() => d.durations.map((p) => ({ t: shortDay(p.t), p50: p.p50Ms ? p.p50Ms / 1000 : null, p95: p.p95Ms ? p.p95Ms / 1000 : null })), [d.durations]);
  const launches = useMemo(() => d.launches.map((p) => ({ t: shortDay(p.t), launches: p.count })), [d.launches]);
  const groups = useMemo(() => d.defectGroups.map((p) => ({ ...p, t: shortDay(p.t) })), [d.defectGroups]);
  const rates = passRate.map((p) => p.passRate).filter((v): v is number => v !== null);
  const passFloor = rates.length ? Math.max(0, Math.floor((Math.min(...rates) - 5) / 10) * 10) : 0;
  const latest = [...d.passRate].reverse().find((p) => p.total > 0);
  const totalLaunches = d.launches.reduce((s, l) => s + l.count, 0);
  const totalResults = d.passRate.reduce((s, p) => s + p.total, 0);
  const defects = DEFECT_GROUPS.map((g) => ({ g, n: d.defectGroups.reduce((s, p) => s + p[g], 0) }));
  const failures = defects.reduce((s, x) => s + x.n, 0);
  const investigate = defects.find((x) => x.g === 'toInvestigate')?.n ?? 0;
  const maxFail = Math.max(1, ...d.mostFailing.map((m) => m.failures));

  return (
    <Stack gap="md">
      <SimpleGrid cols={{ base: 2, md: 4 }} spacing="sm">
        <Stat label="Latest pass rate" value={latest ? `${(latest.passRate * 100).toFixed(1)}%` : '—'} hint="passed / (total − skipped)" />
        <Stat label="Launches" value={String(totalLaunches)} hint="in the selected range" />
        <Stat label="Results" value={totalResults.toLocaleString()} />
        <Stat label="Failures to investigate" value={String(investigate)} hint={`of ${String(failures)} failed or broken`} />
      </SimpleGrid>
      <SimpleGrid cols={{ base: 1, lg: 2 }} spacing="md">
        <Panel title="Pass rate" hint="passed / (total − skipped), per bucket" testId="chart-pass-rate">
          <LineChart
            h={220}
            data={passRate}
            dataKey="t"
            series={[{ name: 'passRate', label: 'Pass rate', color: 'var(--fp-status-passed)' }]}
            curveType="monotone"
            connectNulls
            withDots={passRate.length <= 16}
            yAxisProps={{ domain: [passFloor, 100], width: 48, tickFormatter: (v: number) => `${String(v)}%`, allowDecimals: false }}
            valueFormatter={(v) => `${v.toFixed(1)}%`}
            gridAxis="x"
            tickLine="none"
            strokeWidth={2}
          />
        </Panel>
        <Panel title="Test durations" hint="p50 and p95 of result durations, seconds" testId="chart-durations">
          <LineChart
            h={220}
            data={durations}
            dataKey="t"
            series={[
              { name: 'p50', label: 'p50', color: 'fhPurple.4' },
              { name: 'p95', label: 'p95', color: 'var(--fp-status-broken)' },
            ]}
            curveType="monotone"
            connectNulls
            withLegend
            legendProps={{ verticalAlign: 'top', height: 28 }}
            withDots={durations.length <= 16}
            yAxisProps={{ width: 44 }}
            valueFormatter={(v) => formatDuration(v * 1000)}
            gridAxis="x"
            tickLine="none"
            strokeWidth={2}
          />
        </Panel>
        <Panel title="Launches" hint="launches started per bucket (debug launches excluded)" testId="chart-launches">
          <BarChart
            h={200}
            data={launches}
            dataKey="t"
            series={[{ name: 'launches', label: 'Launches', color: 'fhPurple.5' }]}
            yAxisProps={{ width: 32, allowDecimals: false }}
            gridAxis="x"
            tickLine="none"
            barProps={{ radius: [4, 4, 0, 0] }}
            maxBarWidth={28}
          />
        </Panel>
        <Panel title="Failures by defect group" hint="failed and broken results by their triage" testId="chart-defects">
          <Group gap="md" wrap="wrap" mb="xs" data-testid="defect-legend">
            {DEFECT_GROUPS.map((g) => (
              <Group key={g} gap={6} wrap="nowrap">
                <span aria-hidden style={{ width: 10, height: 10, borderRadius: 3, background: GROUP_COLOR[g], display: 'inline-block' }} />
                <Text size="xs">{GROUP_LABEL[g]}</Text>
              </Group>
            ))}
          </Group>
          <BarChart
            h={200}
            data={groups}
            dataKey="t"
            type="stacked"
            series={DEFECT_GROUPS.map((g: DefectGroup) => ({ name: g, label: GROUP_LABEL[g], color: GROUP_COLOR[g] }))}
            yAxisProps={{ width: 32, allowDecimals: false }}
            gridAxis="x"
            tickLine="none"
            maxBarWidth={28}
          />
        </Panel>
        <Panel title="Flaky tests" hint="highest flakiness score first" testId="flaky-top">
          {d.flakyTop.length === 0 ? (
            <Text size="sm" c="dimmed">
              No flaky tests in this range.
            </Text>
          ) : (
            <Stack gap={8}>
              {d.flakyTop.map((f) => (
                <Group key={f.testCaseId} gap="sm" wrap="nowrap">
                  <Anchor component={Link} to={`/p/${project}/test-cases/${encodeURIComponent(f.testCaseId)}`} size="sm" className="fh-truncate" style={{ flex: 1, minWidth: 0 }}>
                    {f.name}
                  </Anchor>
                  <Tooltip label={`Score ${String(Math.round(f.score * 100))}%`}>
                    <Progress value={f.score * 100} color="var(--fp-status-flaky)" w={90} size="sm" radius="xl" style={{ flex: 'none' }} aria-label={`Flakiness ${String(Math.round(f.score * 100))}%`} />
                  </Tooltip>
                  <Text size="xs" c="dimmed" w={36} ta="right" style={{ flex: 'none' }}>
                    {Math.round(f.score * 100)}%
                  </Text>
                </Group>
              ))}
              <Anchor component={Link} to={`/p/${project}/flaky`} size="xs">
                All flaky tests →
              </Anchor>
            </Stack>
          )}
        </Panel>
        <Panel title="Most failing tests" hint="failed or broken results in the range" testId="most-failing">
          {d.mostFailing.length === 0 ? (
            <Text size="sm" c="dimmed">
              No failures in this range.
            </Text>
          ) : (
            <Stack gap={8}>
              {d.mostFailing.map((m) => (
                <Group key={m.testCaseId} gap="sm" wrap="nowrap">
                  <Anchor component={Link} to={`/p/${project}/test-cases/${encodeURIComponent(m.testCaseId)}`} size="sm" className="fh-truncate" style={{ flex: 1, minWidth: 0 }}>
                    {m.name}
                  </Anchor>
                  <Progress value={(m.failures / maxFail) * 100} color="var(--fp-status-failed)" w={90} size="sm" radius="xl" style={{ flex: 'none' }} aria-hidden />
                  <Text size="xs" c="dimmed" w={36} ta="right" style={{ flex: 'none' }}>
                    {m.failures}
                  </Text>
                </Group>
              ))}
            </Stack>
          )}
        </Panel>
      </SimpleGrid>
    </Stack>
  );
}

export default function DashboardPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const [sp, setSp] = useSearchParams();
  const [defaults] = useState(defaultRange);
  const branch = sp.get('branch') ?? '';
  const environment = sp.get('environment') ?? '';
  const from = sp.get('from') ?? defaults.from;
  const to = sp.get('to') ?? defaults.to;
  const bucket: DashboardBucket = sp.get('bucket') === 'week' ? 'week' : 'day';
  const q = useDashboard(key, { branch: branch || undefined, environment: environment || undefined, from, to, bucket });
  const { launches } = useLaunchList(key);
  const branches = useMemo(() => [...new Set(launches.map((l) => l.vcs?.branch).filter((b): b is string => !!b))], [launches]);
  const envs = useMemo(() => [...new Set(launches.map((l) => l.environment?.key).filter((e): e is string => !!e))], [launches]);

  const set = (k: string, v: string | null) =>
    setSp(
      (prev) => {
        const n = new URLSearchParams(prev);
        if (v) n.set(k, v);
        else n.delete(k);
        return n;
      },
      { replace: true },
    );

  return (
    <>
      <PageHeader crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Dashboard' }]} title="Dashboard" subtitle="Quality trends across launches" />
      <Card withBorder radius="md" padding="sm" mb="md">
        <SimpleGrid cols={{ base: 1, sm: 2, lg: 5 }} spacing="xs">
          <Autocomplete aria-label="Branch" placeholder="All branches" data={branches} value={branch} onChange={(v) => set('branch', v)} clearable />
          <Autocomplete aria-label="Environment" placeholder="All environments" data={envs} value={environment} onChange={(v) => set('environment', v)} clearable />
          <TextInput type="date" aria-label="From" value={from} max={to} onChange={(e) => set('from', e.currentTarget.value || null)} />
          <TextInput type="date" aria-label="To" value={to} min={from} onChange={(e) => set('to', e.currentTarget.value || null)} />
          <SegmentedControl
            aria-label="Bucket"
            value={bucket}
            onChange={(v) => set('bucket', v === 'week' ? 'week' : null)}
            data={[
              { value: 'day', label: 'Day' },
              { value: 'week', label: 'Week' },
            ]}
          />
        </SimpleGrid>
      </Card>
      <QueryState query={q} notFound="Project not found">
        {() => (q.data && hasData(q.data) ? <Content project={key} d={q.data} /> : <EmptyState title="No launches in this range" description="Widen the date range or clear the branch and environment filters." />)}
      </QueryState>
    </>
  );
}
