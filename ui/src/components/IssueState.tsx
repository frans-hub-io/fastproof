import { Badge, Tooltip } from '@mantine/core';
import { IconAlertTriangle, IconCircleCheck, IconCircleDot, IconLoader2 } from '@tabler/icons-react';
import type { TrackerIssue } from '../api/trackers';
import { TRACKER_LABEL } from '../api/trackers';
import { formatDateTime } from '../lib/format';

/** The state of a tracked issue as its tracker last reported it. */
export function IssueStateBadge({ issue, size = 'sm' }: { issue: Pick<TrackerIssue, 'category' | 'state' | 'trackerType' | 'syncedAt' | 'syncError' | 'closedAt'>; size?: 'xs' | 'sm' | 'md' }) {
  const closed = issue.category === 'closed';
  const creating = issue.category === 'creating';
  const label = creating ? 'creating' : issue.state || (closed ? 'closed' : 'open');
  const color = closed ? 'green' : creating ? 'gray' : 'blue';
  const icon = issue.syncError ? <IconAlertTriangle size={11} /> : closed ? <IconCircleCheck size={11} /> : creating ? <IconLoader2 size={11} /> : <IconCircleDot size={11} />;
  const tracker = TRACKER_LABEL[issue.trackerType] ?? issue.trackerType;
  const tip = [
    `${tracker}: ${label}`,
    issue.closedAt && closed ? `closed ${formatDateTime(issue.closedAt)}` : '',
    issue.syncedAt ? `read ${formatDateTime(issue.syncedAt)}` : 'not read yet',
    issue.syncError ? `last read failed: ${issue.syncError}` : '',
  ]
    .filter(Boolean)
    .join(' · ');
  return (
    <Tooltip label={tip} multiline maw={320}>
      <Badge
        size={size}
        radius="sm"
        variant="light"
        color={issue.syncError ? 'orange' : color}
        leftSection={icon}
        styles={{ root: { textTransform: 'none', flex: 'none' } }}
        data-testid="issue-state"
        data-category={issue.category}
      >
        {label}
      </Badge>
    </Tooltip>
  );
}
