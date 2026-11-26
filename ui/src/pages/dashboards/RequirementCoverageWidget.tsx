import { Anchor, Group, Stack, Table, Text } from '@mantine/core';
import { Link } from 'react-router';
import type { WidgetData } from '../../api/dashboards';
import { requirementKey } from '../../api/requirements';
import { CountsText, CoverageChip, CoverageLegend } from '../requirements/coverage';
import { COVERAGE_COLOR, COVERAGE_LABEL, COVERAGE_ORDER } from '../requirements/coverageStyle';

type Data = Extract<WidgetData, { kind: 'requirementCoverage' }>;

function Bar({ data }: { data: Data }) {
  const label = `${String(data.total)} requirements: ${COVERAGE_ORDER.map((s) => `${String(data.counts[s])} ${COVERAGE_LABEL[s].toLowerCase()}`).join(', ')}`;
  return (
    <div role="img" aria-label={label} style={{ display: 'flex', height: 14, borderRadius: 4, overflow: 'hidden', background: 'color-mix(in srgb, var(--fp-status-skipped) 18%, transparent)' }} data-testid="requirement-coverage-bar">
      {COVERAGE_ORDER.map((s) => (data.counts[s] > 0 ? <span key={s} style={{ width: `${String((data.counts[s] / data.total) * 100)}%`, background: COVERAGE_COLOR[s] }} /> : null))}
    </div>
  );
}

export function RequirementCoverageWidget({ project, data }: { project: string; data: Data }) {
  if (data.total === 0) {
    return (
      <Text size="sm" c="dimmed" data-testid="widget-empty">
        No requirements yet.
      </Text>
    );
  }
  return (
    <div data-testid="requirement-coverage" style={{ minWidth: 0 }}>
      <Group justify="space-between" gap="xs" mb={6} wrap="wrap">
        <Text size="xs" c="dimmed">
          {data.total.toLocaleString()} top-level requirements · {data.scope.kind === 'window' ? `last ${String(data.scope.days ?? 30)} days` : 'newest result of each case'}
        </Text>
        <Anchor component={Link} to={`/p/${project}/coverage`} size="xs">
          Open coverage
        </Anchor>
      </Group>
      <Bar data={data} />
      <div style={{ margin: '8px 0' }}>
        <CoverageLegend counts={data.counts} />
      </div>
      <Table verticalSpacing={4} fz="sm" layout="auto" data-testid="requirement-coverage-table">
        <Table.Thead>
          <Table.Tr>
            <Table.Th>Requirement</Table.Th>
            <Table.Th ta="right" style={{ width: '1%', whiteSpace: 'nowrap' }}>
              Coverage
            </Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {data.items.map((r) => (
            <Table.Tr key={r.number}>
              <Table.Td style={{ overflowWrap: 'anywhere' }}>
                <Anchor component={Link} to={`/p/${project}/requirements/${String(r.number)}`} size="sm">
                  <span className="fp-mono">{requirementKey(r.number)}</span> {r.title}
                </Anchor>
              </Table.Td>
              <Table.Td ta="right" style={{ width: '1%', whiteSpace: 'nowrap', verticalAlign: 'top' }} data-testid="requirement-coverage-cell">
                <Stack gap={2} align="flex-end">
                  <CoverageChip status={r.coverage} size="xs" />
                  <CountsText counts={r.cases} />
                </Stack>
              </Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
    </div>
  );
}
