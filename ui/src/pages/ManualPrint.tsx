import { Anchor, Button, Group, Stack, Table, Text, Title } from '@mantine/core';
import { IconArrowLeft, IconPrinter } from '@tabler/icons-react';
import { Link, useParams } from 'react-router';
import { useLaunch, useProject } from '../api/hooks';
import { useManualReport, type LaunchManual, type ManualReportItem, type ManualReportStep, type RunStatus } from '../api/tm';
import type { components } from '../api/schema.gen';
import { QueryState } from '../components/QueryState';
import { formatDateTime } from '../lib/format';
import { caseLabel } from '../lib/tm/filters';
import { valuesLabel } from '../lib/tm/parameters';
import { RUN_STATUS_LABEL } from '../lib/tm/steps';
import { defectInfo, markdownText } from '../lib/tm/ui';
import { RunStatusBadge } from './cases/common';

type ManualReportIteration = components['schemas']['ManualReportIteration'];

const ORDER: RunStatus[] = ['passed', 'failed', 'blocked', 'skipped', 'in_progress', 'untested'];

function StepsTable({ steps }: { steps: ManualReportStep[] }) {
  return (
    <div className="fp-table-wrap fp-print-table">
      <Table withTableBorder verticalSpacing={4} mt={6} miw={560} className="fp-print-steps" data-testid="print-steps">
        <Table.Thead>
          <Table.Tr>
            <Table.Th w={50}>Step</Table.Th>
            <Table.Th>Action</Table.Th>
            <Table.Th>Expected</Table.Th>
            <Table.Th w={110}>Status</Table.Th>
            <Table.Th>Actual result and comment</Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {steps.map((s) => (
            <Table.Tr key={s.path} data-testid="print-step" data-status={s.status}>
              <Table.Td className="fp-mono">{s.path}</Table.Td>
              <Table.Td style={{ whiteSpace: 'pre-wrap' }}>{markdownText(s.action)}</Table.Td>
              <Table.Td style={{ whiteSpace: 'pre-wrap' }}>{markdownText(s.expected)}</Table.Td>
              <Table.Td>
                <RunStatusBadge status={s.status} size="xs" />
              </Table.Td>
              <Table.Td style={{ whiteSpace: 'pre-wrap' }}>
                {markdownText(s.actual)}
                {s.actual && s.comment ? '\n' : ''}
                {s.comment ? <i>{s.comment}</i> : null}
              </Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
    </div>
  );
}

function IterationReport({ columns, x }: { columns: readonly string[]; x: ManualReportIteration }) {
  const defect = defectInfo(x.defect);
  return (
    <div className="fp-print-item" data-testid="print-iteration" data-row={x.rowId} data-status={x.status}>
      <Group justify="space-between" gap="xs" wrap="wrap" align="baseline">
        <Title order={4} fz={14}>
          Iteration #{x.position}
          <Text span size="xs" c="dimmed" ml={6} style={{ wordBreak: 'break-word' }}>
            {valuesLabel(columns, x.values)}
          </Text>
        </Title>
        <RunStatusBadge status={x.status} size="xs" />
      </Group>
      <Text size="xs" c="dimmed">
        executed by {x.executedBy ?? '—'} · started {x.startedAt ? formatDateTime(x.startedAt) : '—'} · finished {x.finishedAt ? formatDateTime(x.finishedAt) : '—'}
      </Text>
      {x.comment && (
        <Text size="sm" style={{ whiteSpace: 'pre-wrap' }}>
          <b>Comment:</b> {x.comment}
        </Text>
      )}
      {defect && (
        <Text size="sm" data-testid="print-iteration-defect">
          <b>Defect:</b> {defect.label}
          {defect.issue ? ' · ' : ''}
          {defect.issue && (defect.url ? <Anchor href={defect.url}>{defect.issue}</Anchor> : defect.issue)}
          {defect.comment ? ` · ${defect.comment}` : ''}
        </Text>
      )}
      {x.steps.length > 0 ? (
        <StepsTable steps={x.steps} />
      ) : (
        <Text size="xs" c="dimmed">
          {x.stepsOmitted ? 'Step results are left out of this report (size limit).' : 'No step results recorded.'}
        </Text>
      )}
    </div>
  );
}

function ItemReport({ it }: { it: ManualReportItem }) {
  const defect = defectInfo(it.defect);
  return (
    <section className="fp-print-item" data-testid="print-item" data-case={it.caseNumber} aria-label={`${caseLabel(it.caseNumber)} ${it.name}`}>
      <Group justify="space-between" gap="xs" wrap="wrap" align="baseline">
        <Title order={3} fz={16}>
          <Text span c="dimmed" className="fp-mono" fz={14} mr={6}>
            {caseLabel(it.caseNumber)}
          </Text>
          {it.name}
        </Title>
        <RunStatusBadge status={it.status} size="sm" />
      </Group>
      <Text size="xs" c="dimmed" data-testid="print-item-meta">
        Version {it.caseVersion} · run {it.executionNo} · assignee {it.assignee ?? 'none'} · executed by {it.executedBy ?? '—'} · started {it.startedAt ? formatDateTime(it.startedAt) : '—'} · finished {it.finishedAt ? formatDateTime(it.finishedAt) : '—'}
      </Text>
      {it.comment && (
        <Text size="sm" mt={4} style={{ whiteSpace: 'pre-wrap' }}>
          <b>Comment:</b> {it.comment}
        </Text>
      )}
      {defect && (
        <Text size="sm" mt={4} style={{ whiteSpace: 'pre-wrap' }} data-testid="print-item-defect">
          <b>Defect:</b> {defect.label}
          {defect.issue ? ' · ' : ''}
          {defect.issue && (defect.url ? <Anchor href={defect.url}>{defect.issue}</Anchor> : defect.issue)}
          {defect.comment ? ` · ${defect.comment}` : ''}
          {defect.by ? ` · by ${defect.by}` : ''}
        </Text>
      )}
      {it.iterations && it.iterations.length > 0 ? (
        <Stack gap="xs" mt={6} data-testid="print-iterations">
          {it.iterations.map((x) => (
            <IterationReport key={x.rowId} columns={it.parameterColumns ?? []} x={x} />
          ))}
        </Stack>
      ) : it.steps.length > 0 ? (
        <StepsTable steps={it.steps} />
      ) : (
        <Text size="xs" c="dimmed">
          No steps.
        </Text>
      )}
    </section>
  );
}

/** The printable manual run report: every item of the run with its steps, independent of any on-screen filter. */
export default function ManualPrintPage() {
  const { key = '', id = '' } = useParams();
  const project = useProject(key);
  const launch = useLaunch(key, id);
  const report = useManualReport(key, id);
  return (
    <QueryState query={report} notFound="No manual run in this launch">
      {() => {
        const r = report.data;
        if (!r) return null;
        const counts = Object.fromEntries(ORDER.map((s) => [s, r.items.filter((i) => i.status === s).length])) as Record<RunStatus, number>;
        const l = launch.data;
        const manual: LaunchManual | null | undefined = l?.manual;
        return (
          <Stack gap="md" className="fp-print-page" data-testid="manual-print-view">
            <Group justify="space-between" className="fp-no-print" gap="xs" wrap="wrap">
              <Button variant="subtle" size="xs" component={Link} to={`/p/${key}/launches/${id}?tab=manual`} leftSection={<IconArrowLeft size={14} />}>
                Back to the launch
              </Button>
              <Button size="xs" leftSection={<IconPrinter size={14} />} onClick={() => window.print()} data-testid="print-now">
                Print
              </Button>
            </Group>
            <Stack gap={4}>
              <Title order={1} fz={22}>
                Manual run report: #{r.launchNumber} {r.launchName}
              </Title>
              <Text size="sm" c="dimmed">
                {project.data?.name ?? key}
                {l?.environment ? ` · ${l.environment.key}` : ''}
                {l?.vcs?.branch ? ` · ${l.vcs.branch}` : ''}
                {l?.manual ? ` · manual run ${l.manual.state}` : ''}
                {manual?.startedAt ? ` · started ${formatDateTime(manual.startedAt)}` : ''}
                {manual?.finishedAt ? ` · last result ${formatDateTime(manual.finishedAt)}` : ''} · printed {formatDateTime(new Date().toISOString())}
              </Text>
              <Text size="sm" data-testid="print-summary">
                {r.items.length} cases:{' '}
                {ORDER.filter((s) => counts[s] > 0)
                  .map((s) => `${String(counts[s])} ${RUN_STATUS_LABEL[s].toLowerCase()}`)
                  .join(', ') || 'none'}
              </Text>
            </Stack>
            {r.items.map((it) => (
              <ItemReport key={`${String(it.caseNumber)}:${String(it.executionNo)}`} it={it} />
            ))}
          </Stack>
        );
      }}
    </QueryState>
  );
}
