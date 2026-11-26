import { Badge, Group, Text } from '@mantine/core';
import { IconAlertTriangle, IconCircleCheck, IconCircleDashed, IconCircleHalf2, IconCircleOff, IconCircleX } from '@tabler/icons-react';
import type { CoverageCounts, CoverageStatus } from '../../api/requirements';
import { CASE_SEGMENTS, COVERAGE_COLOR, COVERAGE_LABEL, COVERAGE_ORDER, countsLabel } from './coverageStyle';

const ICON = { uncovered: IconCircleOff, failed: IconCircleX, notRun: IconCircleDashed, partial: IconCircleHalf2, passed: IconCircleCheck } as const;

export function CoverageChip({ status, size = 'sm' }: { status: CoverageStatus; size?: 'xs' | 'sm' | 'md' }) {
  const Icon = ICON[status];
  const color = COVERAGE_COLOR[status];
  return (
    <Badge
      size={size}
      variant="light"
      radius="sm"
      leftSection={<Icon size={11} aria-hidden />}
      data-coverage={status}
      styles={{ root: { flex: 'none', textTransform: 'none', color: `color-mix(in srgb, ${color} 65%, var(--mantine-color-text))`, background: `color-mix(in srgb, ${color} 14%, transparent)` } }}
    >
      {COVERAGE_LABEL[status]}
    </Badge>
  );
}

/** The cases of a requirement by result, as a proportional bar with a text alternative. */
export function CountsBar({ counts, height = 8 }: { counts: CoverageCounts; height?: number }) {
  const total = counts.total || 1;
  return (
    <div
      role="img"
      aria-label={counts.total === 0 ? 'No cases' : `${String(counts.total)} cases: ${countsLabel(counts)}`}
      style={{ display: 'flex', height, minWidth: 80, borderRadius: 4, overflow: 'hidden', background: 'color-mix(in srgb, var(--fp-status-skipped) 18%, transparent)' }}
      data-testid="counts-bar"
    >
      {CASE_SEGMENTS.map((s) => (counts[s.key] > 0 ? <span key={s.key} style={{ width: `${String((counts[s.key] / total) * 100)}%`, background: s.color }} /> : null))}
    </div>
  );
}

export function CountsText({ counts }: { counts: CoverageCounts }) {
  return (
    <Text size="xs" c="dimmed" component="span">
      {counts.total === 0 ? 'no cases' : `${String(counts.passed)}/${String(counts.total)} passed${counts.failed ? `, ${String(counts.failed)} failed` : ''}`}
    </Text>
  );
}

export function ChangedAfterRun() {
  return (
    <Badge size="xs" variant="outline" color="yellow" radius="sm" leftSection={<IconAlertTriangle size={10} aria-hidden />} title="The requirement changed after its newest result" data-testid="changed-after-run" styles={{ root: { textTransform: 'none', height: 'auto', maxWidth: '100%' }, label: { whiteSpace: 'normal', overflow: 'visible', lineHeight: 1.4 } }}>
      Changed after run
    </Badge>
  );
}

export function CoverageLegend({ counts }: { counts: Record<CoverageStatus, number> }) {
  return (
    <Group gap="md" wrap="wrap" style={{ rowGap: 4 }} data-testid="coverage-legend">
      {COVERAGE_ORDER.map((s) => (
        <Group key={s} gap={6} wrap="nowrap">
          <span aria-hidden style={{ width: 10, height: 10, borderRadius: 3, background: COVERAGE_COLOR[s], display: 'inline-block' }} />
          <Text size="xs">
            {COVERAGE_LABEL[s]}: <b>{counts[s]}</b>
          </Text>
        </Group>
      ))}
    </Group>
  );
}
