import { AreaChart, BarChart, DonutChart, LineChart } from '@mantine/charts';
import { Anchor, Group, Progress, Stack, Table, Text, VisuallyHidden } from '@mantine/core';
import { useMemo, type ReactNode } from 'react';
import { Link } from 'react-router';
import type { ComponentPoint, DashboardWidget, LaunchPoint, WidgetData } from '../../api/dashboards';
import type { DefectGroup } from '../../api/types-next';
import { DEFECT_GROUPS } from '../../api/types-next';
import { SameName } from '../../components/SameName';
import { GROUP_COLOR, GROUP_LABEL } from '../../lib/defects';
import { formatDuration } from '../../lib/format';
import { sameNameHints } from '../../lib/names';

const STATUS_SERIES = [
  { name: 'passed', label: 'Passed', color: 'var(--fp-status-passed)' },
  { name: 'failed', label: 'Failed', color: 'var(--fp-status-failed)' },
  { name: 'broken', label: 'Broken', color: 'var(--fp-status-broken)' },
  { name: 'skipped', label: 'Skipped', color: 'var(--fp-status-skipped)' },
  { name: 'unknown', label: 'Unknown', color: 'var(--fp-status-unknown)' },
] as const;

const ROW = 72;
const chartHeight = (w: DashboardWidget) => Math.max(120, w.h * ROW - 96);
const DEFECT_LEGEND = DEFECT_GROUPS.map((g) => ({ label: GROUP_LABEL[g], color: GROUP_COLOR[g] }));

function shortDay(t: string): string {
  const d = new Date(`${t}T00:00:00Z`);
  return Number.isNaN(d.getTime()) ? t : d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', timeZone: 'UTC' });
}

const passRateOf = (l: Pick<LaunchPoint, 'passed' | 'total' | 'skipped'>): number | null => (l.total - l.skipped > 0 ? Math.round((l.passed / (l.total - l.skipped)) * 1000) / 10 : null);

function Empty({ text = 'No data in this range.' }: { text?: string }) {
  return (
    <Text size="sm" c="dimmed" data-testid="widget-empty">
      {text}
    </Text>
  );
}

/** The data of a chart as a table for screen readers. */
function Alt({ caption, head, rows }: { caption: string; head: string[]; rows: (string | number)[][] }) {
  return (
    <VisuallyHidden>
      <table>
        <caption>{caption}</caption>
        <thead>
          <tr>
            {head.map((h) => (
              <th key={h} scope="col">
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.slice(0, 100).map((r, i) => (
            <tr key={i}>
              {r.map((c, j) => (j === 0 ? <th key={j} scope="row">{c}</th> : <td key={j}>{c}</td>))}
            </tr>
          ))}
        </tbody>
      </table>
    </VisuallyHidden>
  );
}

function Legend({ items }: { items: readonly { label: string; color: string }[] }) {
  return (
    <Group gap="md" wrap="wrap" mb={6} data-testid="widget-legend">
      {items.map((i) => (
        <Group key={i.label} gap={6} wrap="nowrap">
          <span aria-hidden style={{ width: 10, height: 10, borderRadius: 3, background: i.color, display: 'inline-block' }} />
          <Text size="xs">{i.label}</Text>
        </Group>
      ))}
    </Group>
  );
}

function Figure({ label, legend, children }: { label: string; legend?: readonly { label: string; color: string }[]; children: ReactNode }) {
  return (
    <div role="group" aria-label={label} style={{ minWidth: 0 }}>
      {legend && <Legend items={legend} />}
      {children}
    </div>
  );
}

const launchLabel = (l: LaunchPoint) => `#${String(l.number)}`;

function LaunchStatistics({ w, launches }: { w: DashboardWidget; launches: LaunchPoint[] }) {
  const data = launches.map((l) => ({ t: launchLabel(l), passed: l.passed, failed: l.failed, broken: l.broken, skipped: l.skipped, unknown: l.unknown }));
  const series = STATUS_SERIES.map((s) => ({ ...s }));
  const common = { h: chartHeight(w), data, dataKey: 't', series, gridAxis: 'x' as const, tickLine: 'none' as const, yAxisProps: { width: 44, allowDecimals: false } };
  const kind = w.settings.chart ?? 'bar';
  return (
    <Figure label={`Results by status for ${String(launches.length)} launches`} legend={STATUS_SERIES}>
      {kind === 'line' ? <LineChart {...common} curveType="monotone" strokeWidth={2} withDots={data.length <= 16} /> : kind === 'area' ? <AreaChart {...common} type="stacked" curveType="monotone" /> : <BarChart {...common} type="stacked" maxBarWidth={32} />}
      <Alt caption="Results by status per launch" head={['Launch', ...STATUS_SERIES.map((s) => s.label)]} rows={launches.map((l) => [launchLabel(l), l.passed, l.failed, l.broken, l.skipped, l.unknown])} />
    </Figure>
  );
}

function Overall({ w, launches }: { w: DashboardWidget; launches: LaunchPoint[] }) {
  const sums = STATUS_SERIES.map((s) => ({ ...s, value: launches.reduce((n, l) => n + l[s.name], 0) }));
  const total = sums.reduce((n, s) => n + s.value, 0);
  const h = Math.min(chartHeight(w), 220);
  return (
    <Figure label={`Overall statistics of ${String(total)} results`}>
      <Group gap="lg" wrap="wrap" align="center" justify="center">
        <DonutChart size={h} thickness={22} withTooltip data={sums.filter((s) => s.value > 0).map((s) => ({ name: s.label, value: s.value, color: s.color }))} chartLabel={total.toLocaleString()} />
        <Stack gap={4} data-testid="overall-legend">
          {sums.map((s) => (
            <Group key={s.name} gap={8} wrap="nowrap">
              <span aria-hidden style={{ width: 10, height: 10, borderRadius: 3, background: s.color, display: 'inline-block' }} />
              <Text size="sm">
                {s.label}: <b>{s.value.toLocaleString()}</b>
                {total > 0 && (
                  <Text span size="xs" c="dimmed">
                    {' '}
                    ({((s.value / total) * 100).toFixed(1)}%)
                  </Text>
                )}
              </Text>
            </Group>
          ))}
        </Stack>
      </Group>
    </Figure>
  );
}

function Comparison({ w, launches }: { w: DashboardWidget; launches: LaunchPoint[] }) {
  const data = launches.map((l) => ({ t: launchLabel(l), passed: l.passed, failed: l.failed, broken: l.broken, skipped: l.skipped, unknown: l.unknown }));
  return (
    <Figure label="Status shares of launches" legend={STATUS_SERIES}>
      <BarChart h={chartHeight(w)} data={data} dataKey="t" type="percent" series={STATUS_SERIES.map((s) => ({ ...s }))} gridAxis="x" tickLine="none" yAxisProps={{ width: 48 }} maxBarWidth={32} />
      <Alt caption="Results by status per launch" head={['Launch', ...STATUS_SERIES.map((s) => s.label)]} rows={launches.map((l) => [launchLabel(l), l.passed, l.failed, l.broken, l.skipped, l.unknown])} />
    </Figure>
  );
}

function Duration({ w, launches }: { w: DashboardWidget; launches: LaunchPoint[] }) {
  const data = launches.map((l) => ({ t: launchLabel(l), seconds: l.durationMs ? l.durationMs / 1000 : null }));
  return (
    <Figure label="Launch durations">
      <BarChart h={chartHeight(w)} data={data} dataKey="t" series={[{ name: 'seconds', label: 'Duration', color: 'fhPurple.5' }]} valueFormatter={(v) => formatDuration(v * 1000)} gridAxis="x" tickLine="none" yAxisProps={{ width: 56, tickFormatter: (v: number) => formatDuration(v * 1000) }} maxBarWidth={32} />
      <Alt caption="Launch durations" head={['Launch', 'Duration']} rows={launches.map((l) => [launchLabel(l), formatDuration(l.durationMs)])} />
    </Figure>
  );
}

function LaunchesTable({ project, launches }: { project: string; launches: LaunchPoint[] }) {
  return (
    <Table.ScrollContainer minWidth={560}>
      <Table verticalSpacing={4} fz="sm" data-testid="launches-table">
        <Table.Thead>
          <Table.Tr>
            <Table.Th>Launch</Table.Th>
            <Table.Th ta="right">Passed</Table.Th>
            <Table.Th ta="right">Failed</Table.Th>
            <Table.Th ta="right">Broken</Table.Th>
            <Table.Th ta="right">Skipped</Table.Th>
            <Table.Th ta="right">Pass rate</Table.Th>
            <Table.Th ta="right">Duration</Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {[...launches].reverse().map((l) => (
            <Table.Tr key={l.id}>
              <Table.Td>
                <Anchor component={Link} to={`/p/${project}/launches/${l.id}`} size="sm">
                  #{l.number} {l.name}
                </Anchor>
              </Table.Td>
              <Table.Td ta="right">{l.passed}</Table.Td>
              <Table.Td ta="right">{l.failed}</Table.Td>
              <Table.Td ta="right">{l.broken}</Table.Td>
              <Table.Td ta="right">{l.skipped}</Table.Td>
              <Table.Td ta="right">{passRateOf(l) === null ? '—' : `${String(passRateOf(l))}%`}</Table.Td>
              <Table.Td ta="right">{formatDuration(l.durationMs)}</Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
    </Table.ScrollContainer>
  );
}

function NonPassed({ w, launches, only }: { w: DashboardWidget; launches: LaunchPoint[]; only: 'failed' | 'non-passed' }) {
  const data = launches.map((l) => ({ t: launchLabel(l), failed: l.failed, broken: l.broken, skipped: l.skipped }));
  const series = STATUS_SERIES.filter((s) => (only === 'failed' ? s.name === 'failed' || s.name === 'broken' : s.name === 'failed' || s.name === 'broken' || s.name === 'skipped')).map((s) => ({ ...s }));
  const label = only === 'failed' ? 'Failed and broken results per launch' : 'Failed, broken and skipped results per launch';
  return (
    <Figure label={label} legend={series}>
      {only === 'failed' ? (
        <LineChart h={chartHeight(w)} data={data} dataKey="t" series={series} curveType="monotone" strokeWidth={2} withDots={data.length <= 16} gridAxis="x" tickLine="none" yAxisProps={{ width: 44, allowDecimals: false }} />
      ) : (
        <AreaChart h={chartHeight(w)} data={data} dataKey="t" type="stacked" series={series} curveType="monotone" gridAxis="x" tickLine="none" yAxisProps={{ width: 44, allowDecimals: false }} />
      )}
      <Alt caption={label} head={['Launch', ...series.map((s) => s.label)]} rows={launches.map((l) => [launchLabel(l), ...series.map((s) => l[s.name])])} />
    </Figure>
  );
}

function RatePerLaunch({ w, launches }: { w: DashboardWidget; launches: LaunchPoint[] }) {
  const data = launches.map((l) => ({ t: launchLabel(l), rate: passRateOf(l) }));
  return (
    <Figure label="Pass rate per launch">
      <BarChart h={chartHeight(w)} data={data} dataKey="t" series={[{ name: 'rate', label: 'Pass rate', color: 'var(--fp-status-passed)' }]} valueFormatter={(v) => `${v.toFixed(1)}%`} yAxisProps={{ domain: [0, 100], width: 44, tickFormatter: (v: number) => `${String(v)}%` }} gridAxis="x" tickLine="none" maxBarWidth={32} />
      <Alt caption="Pass rate per launch" head={['Launch', 'Pass rate']} rows={launches.map((l) => [launchLabel(l), passRateOf(l) === null ? '—' : `${String(passRateOf(l))}%`])} />
    </Figure>
  );
}

function RateSummary({ launches }: { launches: LaunchPoint[] }) {
  const sum = launches.reduce((a, l) => ({ passed: a.passed + l.passed, total: a.total + l.total, skipped: a.skipped + l.skipped }), { passed: 0, total: 0, skipped: 0 });
  const rate = passRateOf(sum);
  return (
    <Stack gap={2} data-testid="rate-summary">
      <Text fw={800} fz={36} lh={1.1}>
        {rate === null ? '—' : `${rate.toFixed(1)}%`}
      </Text>
      <Text size="xs" c="dimmed">
        {launches.length} launches, {sum.total.toLocaleString()} results
      </Text>
      <Text size="xs" c="dimmed">
        passed / (total − skipped)
      </Text>
    </Stack>
  );
}

function TopList({ rows, color, unit }: { rows: { key: string; to: string; name: string; value: number; max: number; hint?: string }[]; color: string; unit: (v: number) => string }) {
  return (
    <Stack gap={8}>
      {rows.map((r) => (
        <Group key={r.key} gap="sm" wrap="nowrap">
          <Anchor component={Link} to={r.to} size="sm" className="fh-truncate" style={{ flex: 1, minWidth: 0 }}>
            {r.name}
            <SameName hint={r.hint} />
          </Anchor>
          <Progress value={(r.value / Math.max(r.max, 1e-9)) * 100} color={color} w={80} size="sm" radius="xl" style={{ flex: 'none' }} aria-hidden />
          <Text size="xs" c="dimmed" w={44} ta="right" style={{ flex: 'none' }}>
            {unit(r.value)}
          </Text>
        </Group>
      ))}
    </Stack>
  );
}

function Patterns({ items }: { items: Extract<WidgetData, { kind: 'patterns' }>['items'] }) {
  return (
    <Table.ScrollContainer minWidth={480}>
      <Table verticalSpacing={4} fz="sm" data-testid="patterns-table">
        <Table.Thead>
          <Table.Tr>
            <Table.Th>Error</Table.Th>
            <Table.Th ta="right">Occurrences</Table.Th>
            <Table.Th>Last seen</Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {items.map((p) => (
            <Table.Tr key={p.fingerprint}>
              <Table.Td style={{ maxWidth: 520, overflowWrap: 'anywhere' }}>{p.message || '(no message)'}</Table.Td>
              <Table.Td ta="right">{p.occurrences.toLocaleString()}</Table.Td>
              <Table.Td>{new Date(p.lastSeenAt).toLocaleDateString()}</Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
    </Table.ScrollContainer>
  );
}

function LaunchExecution({ project, w, d }: { project: string; w: DashboardWidget; d: Extract<WidgetData, { kind: 'launch' }> }) {
  if (!d.launch) return <Empty text="No launch matches." />;
  const l = d.launch;
  const failures = DEFECT_GROUPS.reduce((n, g: DefectGroup) => n + d.defects[g], 0);
  return (
    <Stack gap="sm" data-testid="launch-execution">
      <Anchor component={Link} to={`/p/${project}/launches/${l.id}`} size="sm" fw={600}>
        #{l.number} {l.name}
      </Anchor>
      <Progress.Root size={14} radius="sm" aria-label={`Statuses of launch ${String(l.number)}`}>
        {STATUS_SERIES.map((s) => (l[s.name] > 0 ? <Progress.Section key={s.name} value={(l[s.name] / Math.max(l.total, 1)) * 100} color={s.color} aria-label={`${s.label}: ${String(l[s.name])}`} /> : null))}
      </Progress.Root>
      <Group gap="md" wrap="wrap">
        {STATUS_SERIES.map((s) => (
          <Group key={s.name} gap={6} wrap="nowrap">
            <span aria-hidden style={{ width: 10, height: 10, borderRadius: 3, background: s.color, display: 'inline-block' }} />
            <Text size="xs">
              {s.label} {l[s.name]}
            </Text>
          </Group>
        ))}
      </Group>
      <Text size="xs" c="dimmed" mt={4}>
        {failures} failed or broken results by defect group
      </Text>
      <Legend items={DEFECT_LEGEND} />
      <BarChart h={Math.min(chartHeight(w), 110)} data={[{ t: 'Defects', ...d.defects }]} dataKey="t" orientation="vertical" type="stacked" series={DEFECT_GROUPS.map((g) => ({ name: g, label: GROUP_LABEL[g], color: GROUP_COLOR[g] }))} withYAxis={false} gridAxis="none" tickLine="none" xAxisProps={{ allowDecimals: false }} />
      <Alt caption="Failures by defect group" head={['Group', 'Results']} rows={DEFECT_GROUPS.map((g) => [GROUP_LABEL[g], d.defects[g]])} />
    </Stack>
  );
}

function Components({ w, d }: { w: DashboardWidget; d: Extract<WidgetData, { kind: 'launch' }> }) {
  const rows: ComponentPoint[] = d.components;
  if (!d.launch || rows.length === 0) return <Empty text="No results with this label in the launch." />;
  const rate = (c: ComponentPoint) => passRateOf(c);
  if (w.settings.view === 'table') {
    return (
      <Table.ScrollContainer minWidth={420}>
        <Table verticalSpacing={4} fz="sm" data-testid="component-table">
          <Table.Thead>
            <Table.Tr>
              <Table.Th>{d.groupBy}</Table.Th>
              <Table.Th ta="right">Total</Table.Th>
              <Table.Th ta="right">Failed</Table.Th>
              <Table.Th ta="right">Broken</Table.Th>
              <Table.Th ta="right">Pass rate</Table.Th>
            </Table.Tr>
          </Table.Thead>
          <Table.Tbody>
            {rows.map((c) => (
              <Table.Tr key={c.name}>
                <Table.Td style={{ overflowWrap: 'anywhere' }}>{c.name}</Table.Td>
                <Table.Td ta="right">{c.total}</Table.Td>
                <Table.Td ta="right">{c.failed}</Table.Td>
                <Table.Td ta="right">{c.broken}</Table.Td>
                <Table.Td ta="right">{rate(c) === null ? '—' : `${String(rate(c))}%`}</Table.Td>
              </Table.Tr>
            ))}
          </Table.Tbody>
        </Table>
      </Table.ScrollContainer>
    );
  }
  const data = rows.map((c) => ({ t: c.name, passed: c.passed, failed: c.failed, broken: c.broken, skipped: c.skipped }));
  return (
    <Figure label={`Results per ${d.groupBy}`} legend={STATUS_SERIES.filter((x) => x.name !== 'unknown')}>
      <BarChart h={Math.max(chartHeight(w), rows.length * 28 + 40)} data={data} dataKey="t" orientation="vertical" type="percent" series={STATUS_SERIES.filter((s) => s.name !== 'unknown').map((s) => ({ ...s }))} gridAxis="y" tickLine="none" yAxisProps={{ width: 110 }} />
      <Alt caption={`Results per ${d.groupBy}`} head={[d.groupBy, 'Passed', 'Failed', 'Broken', 'Skipped']} rows={rows.map((c) => [c.name, c.passed, c.failed, c.broken, c.skipped])} />
    </Figure>
  );
}

function Trend({ project, w, d }: { project: string; w: DashboardWidget; d: Extract<WidgetData, { kind: 'trend' }>['trend'] }) {
  const h = chartHeight(w);
  const flakyHints = useMemo(() => sameNameHints(d.flakyTop, (f) => f.name, (f) => [f.testCaseId, f.seriesKey]), [d.flakyTop]);
  const failingHints = useMemo(() => sameNameHints(d.mostFailing, (m) => m.name, (m) => [m.testCaseId]), [d.mostFailing]);
  const axis = { gridAxis: 'x' as const, tickLine: 'none' as const };
  switch (w.type) {
    case 'pass-rate-trend': {
      if (d.passRate.length === 0) return <Empty />;
      const data = d.passRate.map((p) => ({ t: shortDay(p.t), passRate: p.total > 0 && p.passRate != null ? Math.round(p.passRate * 1000) / 10 : null }));
      const rates = data.map((p) => p.passRate).filter((v): v is number => v !== null);
      const floor = rates.length ? Math.max(0, Math.floor((Math.min(...rates) - 5) / 10) * 10) : 0;
      return (
        <Figure label="Pass rate per bucket">
          <LineChart h={h} data={data} dataKey="t" series={[{ name: 'passRate', label: 'Pass rate', color: 'var(--fp-status-passed)' }]} curveType="monotone" connectNulls withDots={data.length <= 16} yAxisProps={{ domain: [floor, 100], width: 56, tickFormatter: (v: number) => `${String(v)}%`, allowDecimals: false }} valueFormatter={(v) => `${v.toFixed(1)}%`} strokeWidth={2} {...axis} />
          <Alt caption="Pass rate per bucket" head={['Day', 'Pass rate']} rows={data.map((p) => [p.t, p.passRate === null ? '—' : `${String(p.passRate)}%`])} />
        </Figure>
      );
    }
    case 'durations': {
      if (d.durations.length === 0) return <Empty />;
      const data = d.durations.map((p) => ({ t: shortDay(p.t), p50: p.p50Ms ? p.p50Ms / 1000 : null, p95: p.p95Ms ? p.p95Ms / 1000 : null }));
      return (
        <Figure label="p50 and p95 durations" legend={[{ label: 'p50', color: 'var(--mantine-color-fhPurple-4)' }, { label: 'p95', color: 'var(--fp-status-broken)' }]}>
          <LineChart h={h} data={data} dataKey="t" series={[{ name: 'p50', label: 'p50', color: 'fhPurple.4' }, { name: 'p95', label: 'p95', color: 'var(--fp-status-broken)' }]} curveType="monotone" connectNulls withDots={data.length <= 16} yAxisProps={{ width: 56 }} valueFormatter={(v) => formatDuration(v * 1000)} strokeWidth={2} {...axis} />
          <Alt caption="Durations" head={['Day', 'p50', 'p95']} rows={d.durations.map((p) => [shortDay(p.t), formatDuration(p.p50Ms), formatDuration(p.p95Ms)])} />
        </Figure>
      );
    }
    case 'defect-trend': {
      if (d.defectGroups.length === 0) return <Empty />;
      const data = d.defectGroups.map((p) => ({ ...p, t: shortDay(p.t) }));
      return (
        <Figure label="Failures by defect group per bucket" legend={DEFECT_LEGEND}>
          <BarChart h={h} data={data} dataKey="t" type="stacked" series={DEFECT_GROUPS.map((g: DefectGroup) => ({ name: g, label: GROUP_LABEL[g], color: GROUP_COLOR[g] }))} yAxisProps={{ width: 44, allowDecimals: false }} maxBarWidth={28} {...axis} />
          <Alt caption="Failures by defect group" head={['Day', ...DEFECT_GROUPS.map((g) => GROUP_LABEL[g])]} rows={data.map((p) => [p.t, ...DEFECT_GROUPS.map((g) => p[g])])} />
        </Figure>
      );
    }
    case 'investigated-percentage': {
      const data = d.defectGroups
        .map((p) => {
          const failures = DEFECT_GROUPS.reduce((n, g) => n + p[g], 0);
          return { t: shortDay(p.t), investigated: failures > 0 ? Math.round(((failures - p.toInvestigate) / failures) * 1000) / 10 : null };
        })
        .filter((p) => p.investigated !== null);
      if (data.length === 0) return <Empty text="No failures in this range." />;
      return (
        <Figure label="Share of failures that were triaged">
          <AreaChart h={h} data={data} dataKey="t" series={[{ name: 'investigated', label: 'Investigated', color: 'fhPurple.4' }]} curveType="monotone" connectNulls withDots={data.length <= 16} yAxisProps={{ domain: [0, 100], width: 56, tickFormatter: (v: number) => `${String(v)}%` }} valueFormatter={(v) => `${v.toFixed(1)}%`} {...axis} />
          <Alt caption="Investigated share" head={['Day', 'Investigated']} rows={data.map((p) => [p.t, `${String(p.investigated)}%`])} />
        </Figure>
      );
    }
    case 'launches-per-day': {
      if (d.launches.length === 0) return <Empty />;
      const data = d.launches.map((p) => ({ t: shortDay(p.t), launches: p.count }));
      return (
        <Figure label="Launches per bucket">
          <BarChart h={h} data={data} dataKey="t" series={[{ name: 'launches', label: 'Launches', color: 'fhPurple.5' }]} yAxisProps={{ width: 40, allowDecimals: false }} maxBarWidth={28} {...axis} />
          <Alt caption="Launches per bucket" head={['Day', 'Launches']} rows={data.map((p) => [p.t, p.launches])} />
        </Figure>
      );
    }
    case 'flaky-tests':
      if (d.flakyTop.length === 0) return <Empty text="No flaky tests." />;
      return <TopList color="var(--fp-status-flaky)" unit={(v) => `${String(Math.round(v * 100))}%`} rows={d.flakyTop.map((f) => ({ key: f.seriesKey, to: `/p/${project}/test-cases/${encodeURIComponent(f.testCaseId)}`, name: f.name, value: f.score, max: 1, hint: flakyHints.get(f) }))} />;
    case 'most-failing-tests': {
      if (d.mostFailing.length === 0) return <Empty text="No failures in this range." />;
      const max = Math.max(...d.mostFailing.map((m) => m.failures));
      return <TopList color="var(--fp-status-failed)" unit={String} rows={d.mostFailing.map((m) => ({ key: m.testCaseId, to: `/p/${project}/test-cases/${encodeURIComponent(m.testCaseId)}`, name: m.name, value: m.failures, max, hint: failingHints.get(m) }))} />;
    }
    default:
      return <Empty text="This widget shows no data of this kind." />;
  }
}

/** The body of a widget for its data; a type and data that do not belong together show a notice. */
export function WidgetBody({ project, widget, data }: { project: string; widget: DashboardWidget; data: WidgetData }) {
  if (data.kind === 'launches') {
    const l = data.launches;
    if (l.length === 0) return <Empty text="No launches in this range." />;
    switch (widget.type) {
      case 'launch-statistics':
        return <LaunchStatistics w={widget} launches={l} />;
      case 'overall-statistics':
        return <Overall w={widget} launches={l} />;
      case 'launches-comparison':
        return <Comparison w={widget} launches={l} />;
      case 'launches-duration':
        return <Duration w={widget} launches={l} />;
      case 'launches-table':
        return <LaunchesTable project={project} launches={l} />;
      case 'failed-trend':
        return <NonPassed w={widget} launches={l} only="failed" />;
      case 'non-passed-trend':
        return <NonPassed w={widget} launches={l} only="non-passed" />;
      case 'passing-rate-per-launch':
        return <RatePerLaunch w={widget} launches={l} />;
      case 'passing-rate-summary':
        return <RateSummary launches={l} />;
      default:
        return <Empty text="This widget shows no data of this kind." />;
    }
  }
  if (data.kind === 'trend') return <Trend project={project} w={widget} d={data.trend} />;
  if (data.kind === 'growth') {
    if (data.points.length === 0) return <Empty />;
    const total = data.points.reduce((n, p) => n + p.added, 0);
    const rows = data.points.map((p) => ({ t: shortDay(p.t), added: p.added }));
    return (
      <Figure label={`${String(total)} new test cases`}>
        <Text size="xs" c="dimmed" mb={4}>
          {total.toLocaleString()} new test cases in the range
        </Text>
        <BarChart h={chartHeight(widget) - 16} data={rows} dataKey="t" series={[{ name: 'added', label: 'New test cases', color: 'fhPurple.5' }]} gridAxis="x" tickLine="none" yAxisProps={{ width: 44, allowDecimals: false }} maxBarWidth={28} />
        <Alt caption="New test cases" head={['Day', 'New']} rows={rows.map((p) => [p.t, p.added])} />
      </Figure>
    );
  }
  if (data.kind === 'patterns') return data.items.length === 0 ? <Empty text="No failure patterns in this range." /> : <Patterns items={data.items} />;
  return widget.type === 'component-health' ? <Components w={widget} d={data} /> : <LaunchExecution project={project} w={widget} d={data} />;
}
