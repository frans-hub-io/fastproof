import { Badge, Group, Text, Tooltip } from '@mantine/core';
import type { Counts, LaunchStatus as LaunchStatusValue, ResultStatus } from '../api/types';
import { countsLabel } from '../lib/counts';
import { percent } from '../lib/format';
import { LAUNCH_STATUS_LABEL, launchStatusColor, STATUS_LABEL, statusVar } from '../lib/status';

export function StatusDot({ status, size = 10 }: { status: string; size?: number }) {
  return <span aria-hidden style={{ display: 'inline-block', width: size, height: size, borderRadius: '50%', background: statusVar(status), flex: 'none' }} />;
}

export function StatusBadge({ status, size = 'sm' }: { status: ResultStatus | 'flaky'; size?: 'xs' | 'sm' | 'md' }) {
  const label = status === 'flaky' ? 'Flaky' : STATUS_LABEL[status];
  return (
    <Badge
      size={size}
      variant="light"
      radius="sm"
      leftSection={<StatusDot status={status} size={8} />}
      styles={{ root: { color: statusVar(status), background: `color-mix(in srgb, ${statusVar(status)} 14%, transparent)`, textTransform: 'none' } }}
      data-status={status}
    >
      {label}
    </Badge>
  );
}

export function LaunchStatusBadge({ status }: { status: LaunchStatusValue }) {
  return (
    <Badge size="sm" variant="light" color={launchStatusColor(status)} radius="sm" style={{ textTransform: 'none', flex: 'none' }} data-launch-status={status}>
      {LAUNCH_STATUS_LABEL[status]}
    </Badge>
  );
}

export function StatusBar({ counts, height = 8, live = false }: { counts: Counts; height?: number; live?: boolean }) {
  const total = counts.total || 1;
  const order: ResultStatus[] = ['passed', 'failed', 'broken', 'skipped', 'unknown'];
  return (
    <div className={`fp-statusbar${live ? ' fp-progress-stripes' : ''}`} style={{ height }} role="img" aria-label={counts.total ? `${String(counts.total)} results: ${countsLabel(counts)}` : 'No results yet'}>
      {order
        .filter((s) => counts[s] > 0)
        .map((s) => (
          <Tooltip key={s} label={`${STATUS_LABEL[s]}: ${String(counts[s])} (${percent(counts[s], counts.total)})`}>
            <span style={{ width: `${String((counts[s] / total) * 100)}%`, background: statusVar(s) }} className={live ? 'fp-progress-stripes' : undefined} />
          </Tooltip>
        ))}
    </div>
  );
}

export function CountsInline({ counts }: { counts: Counts }) {
  const order: (ResultStatus | 'flaky')[] = ['passed', 'failed', 'broken', 'skipped', 'unknown', 'flaky'];
  return (
    <Group gap={10} wrap="wrap">
      {order
        .filter((s) => counts[s] > 0)
        .map((s) => (
          <Group key={s} gap={4} wrap="nowrap">
            <StatusDot status={s} size={8} />
            <Text size="xs" c="dimmed">
              {String(counts[s])} {s}
            </Text>
          </Group>
        ))}
    </Group>
  );
}
