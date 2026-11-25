import { Anchor, Badge, Group, Text, Tooltip } from '@mantine/core';
import { IconExternalLink } from '@tabler/icons-react';
import { ERROR_STATE_LABEL, type ErrorState, type ErrorSummary } from '../../api/errors';
import { isHttpUrl } from '../../lib/format';

const STATE_COLOR: Record<ErrorState, string> = {
  new: 'red',
  regressed: 'orange',
  active: 'yellow',
  resolved: 'green',
  ignored: 'gray',
};

const STATE_HINT: Record<ErrorState, string> = {
  new: 'Seen in one launch so far',
  active: 'Seen again after it first appeared',
  regressed: 'Came back after it was resolved',
  resolved: 'Absent from the last launches of every branch it occurred on, or its issue was closed',
  ignored: 'Ignored by a person: left out of new errors and notifications',
};

/** The state of an error as a badge with what it means. */
export function ErrorStateBadge({ state, size = 'sm' }: { state: ErrorState; size?: 'xs' | 'sm' | 'md' }) {
  return (
    <Tooltip label={STATE_HINT[state]} multiline w={260}>
      <Badge color={STATE_COLOR[state]} variant="light" size={size} data-testid="error-state" style={{ flex: 'none' }}>
        {ERROR_STATE_LABEL[state]}
      </Badge>
    </Tooltip>
  );
}

/** Daily counts as small bars, oldest first. */
export function Sparkline({ values, label }: { values: number[]; label: string }) {
  const max = Math.max(1, ...values);
  const w = 4;
  const gap = 1;
  const h = 18;
  return (
    <svg width={values.length * (w + gap)} height={h} role="img" aria-label={label} data-testid="error-spark" style={{ display: 'block' }}>
      {values.map((v, i) => {
        const bh = v > 0 ? Math.max(2, Math.round((v / max) * h)) : 1;
        return <rect key={i} x={i * (w + gap)} y={h - bh} width={w} height={bh} rx={1} fill={v > 0 ? 'var(--fp-status-failed)' : 'var(--fh-ui-hairline, #8884)'} />;
      })}
    </svg>
  );
}

/** Branches as badges: the first `max`, then "+n". */
export function BranchBadges({ branches, max = 3 }: { branches: string[]; max?: number }) {
  const shown = branches.slice(0, max);
  const rest = branches.length - shown.length;
  return (
    <Group gap={4} wrap="wrap">
      {shown.map((b) => (
        <Badge key={b} variant="outline" color="gray" size="xs" tt="none" maw={160} title={b === '*' ? 'Other branches (not tracked by name)' : b}>
          {b === '*' ? 'other branches' : b || 'no branch'}
        </Badge>
      ))}
      {rest > 0 && (
        <Tooltip label={branches.slice(max).join(', ')} multiline maw={320}>
          <Text size="xs" c="dimmed">
            +{rest}
          </Text>
        </Tooltip>
      )}
    </Group>
  );
}

/** The issue linked to an error. */
export function IssueLink({ issue }: { issue: ErrorSummary['issue'] }) {
  if (!issue) return <Text size="xs" c="dimmed">—</Text>;
  const label = issue.key ?? 'Issue';
  return (
    <Group gap={4} wrap="wrap">
      {isHttpUrl(issue.url) ? (
        <Anchor href={issue.url} target="_blank" rel="noopener noreferrer" size="xs">
          {label} <IconExternalLink size={11} />
        </Anchor>
      ) : (
        <Text size="xs">{label}</Text>
      )}
      <Badge size="xs" variant="dot" color={issue.state === 'closed' ? 'green' : issue.state === 'open' ? 'blue' : 'gray'}>
        {issue.state}
      </Badge>
    </Group>
  );
}
