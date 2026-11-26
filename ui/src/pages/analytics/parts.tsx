import { BarChart, LineChart, Sparkline } from '@mantine/charts';
import { Alert, Anchor, Card, Group, Progress, Select, SimpleGrid, Stack, Table, Text, Tooltip } from '@mantine/core';
import { IconDatabase } from '@tabler/icons-react';
import type { ReactNode } from 'react';
import { Link } from 'react-router';
import {
  formatSeconds,
  GROUP_BY_LABEL,
  windowNote,
  type AnalyticsWindow,
  type DurationRegressions,
  type FailureAnalytics,
  type FailureGroupBy,
  type FlakyTrend,
  type MttrStats,
  type Portfolio,
  type RetryCosts,
  type SlowSteps,
} from '../../api/analytics';
import { DEFECT_GROUPS, type DefectGroup } from '../../api/types-next';
import { GROUP_COLOR, GROUP_LABEL } from '../../lib/defects';
import { formatDate, formatDuration } from '../../lib/format';

/** The PostgreSQL banner: shown while the answers come from PostgreSQL. */
export function BackendBanner({ w }: { w: AnalyticsWindow | undefined }) {
  if (!w || w.backend !== 'postgres') return null;
  return (
    <Alert variant="light" color="blue" icon={<IconDatabase size={16} />} data-testid="analytics-pg-banner">
      Last 30 days; longer history and the portfolio need ClickHouse.
    </Alert>
  );
}

/** A card with a title, the backend of its answer and a body. */
export function Section({ title, w, testId, children, right }: { title: string; w?: AnalyticsWindow; testId: string; children: ReactNode; right?: ReactNode }) {
  return (
    <Card withBorder radius="md" padding="sm" data-testid={testId}>
      <Group justify="space-between" mb={8} wrap="wrap" gap="xs">
        <Text size="sm" fw={600}>
          {title}
        </Text>
        <Group gap="xs" wrap="wrap">
          {right}
          {w && (
            <Text size="xs" c="dimmed" data-testid={`${testId}-backend`}>
              {windowNote(w)}
            </Text>
          )}
        </Group>
      </Group>
      {children}
    </Card>
  );
}

function Empty({ children }: { children: ReactNode }) {
  return (
    <Text size="sm" c="dimmed" py="sm">
      {children}
    </Text>
  );
}

function shortDay(iso: string): string {
  return formatDate(iso).replace(/, \d{4}$/, '');
}

/** Top failure groups with their share and trend. */
export function TopFailures({ data, projectKey, groupBy, onGroupBy }: { data: FailureAnalytics; projectKey: string; groupBy: FailureGroupBy; onGroupBy: (g: FailureGroupBy) => void }) {
  const picker = (
    <Select
      size="xs"
      w={150}
      aria-label="Group failures by"
      value={groupBy}
      allowDeselect={false}
      onChange={(v) => v && onGroupBy(v)}
      data={(Object.keys(GROUP_BY_LABEL) as FailureGroupBy[]).map((g) => ({ value: g, label: GROUP_BY_LABEL[g] }))}
      data-testid="analytics-group-by"
    />
  );
  return (
    <Section title="Top failures" w={data.window} testId="analytics-failures" right={picker}>
      <Text size="xs" c="dimmed" mb={6}>
        {data.total} failed or broken results in the window
      </Text>
      {data.items.length === 0 ? (
        <Empty>No failures in this window.</Empty>
      ) : (
        <Table.ScrollContainer minWidth={520}>
          <Table verticalSpacing={6} fz="sm">
            <Table.Thead>
              <Table.Tr>
                <Table.Th>{GROUP_BY_LABEL[groupBy]}</Table.Th>
                <Table.Th ta="right">Failures</Table.Th>
                <Table.Th ta="right">Tests</Table.Th>
                <Table.Th ta="right">Launches</Table.Th>
                <Table.Th w={96}>Trend</Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {data.items.map((g) => (
                <Table.Tr key={g.key} data-testid="analytics-failure-row">
                  <Table.Td maw={420}>
                    <Stack gap={4}>
                      {groupBy === 'fingerprint' ? (
                        <Anchor component={Link} to={`/p/${projectKey}/errors/${encodeURIComponent(g.key)}`} size="sm" lineClamp={2}>
                          {g.label}
                        </Anchor>
                      ) : (
                        <Text size="sm" lineClamp={2} c={groupBy === 'defectGroup' ? GROUP_COLOR[g.key as DefectGroup] : undefined}>
                          {g.label}
                        </Text>
                      )}
                      <Tooltip label={`${String(Math.round(g.share * 1000) / 10)}% of all failures`}>
                        <Progress value={g.share * 100} size="xs" color="var(--fp-status-failed)" aria-label="share" />
                      </Tooltip>
                    </Stack>
                  </Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{g.failures}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{g.tests}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{g.launches}</Table.Td>
                  <Table.Td>
                    <Sparkline w={88} h={24} data={g.trend} curveType="linear" color="var(--fp-status-failed)" fillOpacity={0.25} strokeWidth={1.5} />
                  </Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      )}
    </Section>
  );
}

/** Failures per defect group over the window's trend buckets, stacked. */
export function DefectGroupsChart({ data }: { data: FailureAnalytics }) {
  const n = Math.max(0, ...data.items.map((g) => g.trend.length));
  const from = Date.parse(data.window.from);
  const span = Date.parse(data.window.to) - from;
  const rows = Array.from({ length: n }, (_, i) => {
    const row: Record<string, string | number> = { t: shortDay(new Date(from + (span * i) / Math.max(1, n)).toISOString()) };
    for (const g of data.items) row[g.key] = g.trend[i] ?? 0;
    return row;
  });
  const groups = DEFECT_GROUPS.filter((g) => data.items.some((i) => i.key === g));
  return (
    <Section title="Defect groups over time" w={data.window} testId="analytics-defects">
      {data.items.length === 0 ? (
        <Empty>No failures in this window.</Empty>
      ) : (
        <BarChart
          h={220}
          data={rows}
          dataKey="t"
          type="stacked"
          series={groups.map((g) => ({ name: g, label: GROUP_LABEL[g], color: GROUP_COLOR[g] }))}
          withLegend
          legendProps={{ verticalAlign: 'top', height: 40 }}
          tickLine="none"
          gridAxis="y"
          withTooltip
        />
      )}
    </Section>
  );
}

/** Share of flaky results per day. */
export function FlakyTrendChart({ data }: { data: FlakyTrend }) {
  const rows = data.items.map((p) => ({ t: shortDay(p.day), share: p.runs ? Math.round((p.flaky / p.runs) * 1000) / 10 : null, flaky: p.flaky }));
  const total = data.items.reduce((a, p) => a + p.flaky, 0);
  return (
    <Section title="Flaky results" w={data.window} testId="analytics-flaky">
      <Text size="xs" c="dimmed" mb={6}>
        {total} results passed only after a retry
      </Text>
      {data.items.every((p) => p.runs === 0) ? (
        <Empty>No runs in this window.</Empty>
      ) : (
        <LineChart
          h={200}
          data={rows}
          dataKey="t"
          series={[{ name: 'share', label: 'Flaky share', color: 'var(--fp-status-flaky)' }]}
          curveType="monotone"
          connectNulls
          withDots={rows.length <= 31}
          yAxisProps={{ width: 44, tickFormatter: (v: number) => `${String(v)}%` }}
          valueFormatter={(v) => `${v.toFixed(1)}%`}
          tickLine="none"
          gridAxis="y"
          strokeWidth={2}
        />
      )}
    </Section>
  );
}

function Stat({ label, value, testId }: { label: string; value: ReactNode; testId?: string }) {
  return (
    <Stack gap={0}>
      <Text size="xs" c="dimmed">
        {label}
      </Text>
      <Text size="lg" fw={600} data-testid={testId}>
        {value}
      </Text>
    </Stack>
  );
}

const MTTR_LABEL = ['≤ 1 h', '≤ 6 h', '≤ 1 d', '≤ 3 d', '≤ 7 d', '≤ 30 d', '≤ 90 d'];

/** Time to resolve unique errors. */
export function MttrCard({ data }: { data: MttrStats }) {
  const rows = data.buckets.map((b, i) => ({ t: MTTR_LABEL[i] ?? `≤ ${formatSeconds(b.le)}`, errors: b.count - (i > 0 ? (data.buckets[i - 1]?.count ?? 0) : 0) }));
  return (
    <Section title="Time to resolve errors" w={data.window} testId="analytics-mttr">
      <SimpleGrid cols={3} mb="sm">
        <Stat label="Resolved" value={data.resolved} testId="analytics-mttr-resolved" />
        <Stat label="Median" value={formatSeconds(data.p50Seconds)} testId="analytics-mttr-p50" />
        <Stat label="90th percentile" value={formatSeconds(data.p90Seconds)} />
      </SimpleGrid>
      {data.resolved === 0 ? (
        <Empty>No errors resolved in this window.</Empty>
      ) : (
        <BarChart h={160} data={rows} dataKey="t" series={[{ name: 'errors', label: 'Errors', color: 'var(--fp-status-passed)' }]} tickLine="none" gridAxis="y" withTooltip />
      )}
    </Section>
  );
}

/** What retries cost per suite. */
export function RetriesCard({ data }: { data: RetryCosts }) {
  return (
    <Section title="Retry cost" w={data.window} testId="analytics-retries">
      <Text size="sm" mb={6}>
        <Text span fw={600} data-testid="analytics-retries-total">
          {formatDuration(data.totalWastedMs)}
        </Text>{' '}
        spent on attempts that did not count
      </Text>
      {data.items.length === 0 ? (
        <Empty>No retries in this window.</Empty>
      ) : (
        <Table.ScrollContainer minWidth={360}>
          <Table verticalSpacing={4} fz="sm">
            <Table.Thead>
              <Table.Tr>
                <Table.Th>Suite</Table.Th>
                <Table.Th ta="right">Time</Table.Th>
                <Table.Th ta="right">Retries</Table.Th>
                <Table.Th ta="right">Tests</Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {data.items.map((r) => (
                <Table.Tr key={r.suite}>
                  <Table.Td>{r.suite || '(no suite)'}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{formatDuration(r.wastedMs)}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{r.retries}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{r.tests}</Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      )}
    </Section>
  );
}

/** The slowest step names. */
export function SlowStepsCard({ data }: { data: SlowSteps }) {
  return (
    <Section title="Slow steps" w={data.window} testId="analytics-slow-steps">
      {data.items.length === 0 ? (
        <Empty>No steps with a duration in this window.</Empty>
      ) : (
        <Table.ScrollContainer minWidth={360}>
          <Table verticalSpacing={4} fz="sm">
            <Table.Thead>
              <Table.Tr>
                <Table.Th>Step</Table.Th>
                <Table.Th ta="right">p50</Table.Th>
                <Table.Th ta="right">p95</Table.Th>
                <Table.Th ta="right">Runs</Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {data.items.map((s) => (
                <Table.Tr key={s.name}>
                  <Table.Td maw={260}>
                    <Text size="sm" lineClamp={1}>
                      {s.name}
                    </Text>
                  </Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{formatDuration(s.p50Ms)}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{formatDuration(s.p95Ms)}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{s.count}</Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      )}
    </Section>
  );
}

/** Tests that became slower. */
export function DurationsCard({ data, projectKey }: { data: DurationRegressions; projectKey: string }) {
  return (
    <Section title="Slower tests" w={data.window} testId="analytics-durations">
      <Text size="xs" c="dimmed" mb={6}>
        Median of the last 7 days at least 1.3 times the median before, with 10 or more runs in each
      </Text>
      {data.items.length === 0 ? (
        <Empty>No test became slower.</Empty>
      ) : (
        <Table.ScrollContainer minWidth={420}>
          <Table verticalSpacing={4} fz="sm">
            <Table.Thead>
              <Table.Tr>
                <Table.Th>Test</Table.Th>
                <Table.Th ta="right">Before</Table.Th>
                <Table.Th ta="right">Now</Table.Th>
                <Table.Th ta="right">×</Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {data.items.map((d) => (
                <Table.Tr key={d.seriesId} data-testid="analytics-duration-row">
                  <Table.Td maw={260}>
                    <Anchor component={Link} to={`/p/${projectKey}/test-cases/${encodeURIComponent(d.testCaseId)}`} size="sm" lineClamp={1}>
                      {d.name}
                    </Anchor>
                  </Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{formatDuration(d.p50BeforeMs)}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{formatDuration(d.p50RecentMs)}</Table.Td>
                  <Table.Td ta="right" fw={600} c="var(--fp-status-broken)">
                    {d.ratio.toFixed(2)}
                  </Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      )}
    </Section>
  );
}

function pct(v: number | null | undefined): string {
  return v === null || v === undefined ? '—' : `${(v * 100).toFixed(1)}%`;
}

/** The portfolio of the projects the person can read. */
export function PortfolioTable({ data }: { data: Portfolio }) {
  return (
    <Section title="Projects" w={data.window} testId="analytics-portfolio">
      {data.items.length === 0 ? (
        <Empty>No project to show.</Empty>
      ) : (
        <Table.ScrollContainer minWidth={760}>
          <Table verticalSpacing={6} fz="sm" highlightOnHover>
            <Table.Thead>
              <Table.Tr>
                <Table.Th>Project</Table.Th>
                <Table.Th ta="right">Launches</Table.Th>
                <Table.Th ta="right">Pass rate</Table.Th>
                <Table.Th ta="right">Failures</Table.Th>
                <Table.Th ta="right">New errors</Table.Th>
                <Table.Th ta="right">Regressed</Table.Th>
                <Table.Th ta="right">Flaky tests</Table.Th>
                <Table.Th ta="right">Time to resolve</Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {data.items.map((p) => (
                <Table.Tr key={p.key} data-testid="analytics-portfolio-row">
                  <Table.Td>
                    <Anchor component={Link} to={`/p/${p.key}/analytics`} size="sm">
                      {p.name}
                    </Anchor>
                  </Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{p.launches}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{pct(p.passRate)}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{p.failures}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{p.newErrors}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{p.regressedErrors}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{p.flakyTests}</Table.Td>
                  <Table.Td ta="right" style={{ whiteSpace: 'nowrap' }}>{formatSeconds(p.mttrP50Seconds)}</Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      )}
    </Section>
  );
}

