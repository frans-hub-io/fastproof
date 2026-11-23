import { Anchor, Group, Text } from '@mantine/core';
import { IconBug, IconExternalLink } from '@tabler/icons-react';
import { Link } from 'react-router';
import { useIssueStates } from '../api/trackers';
import { IssueStateBadge } from './IssueState';

/** The issue link of a defect with the state its tracker last reported, when FastProof follows it. */
export function DefectIssue({ project, url, issueKey }: { project: string; url: string; issueKey?: string | null }) {
  const issue = useIssueStates(project, [url]).get(url);
  return (
    <Group gap="xs" wrap="wrap" data-testid="defect-issue-row">
      <Anchor href={url} target="_blank" rel="noopener noreferrer nofollow" size="sm" data-testid="defect-issue">
        <Group gap={4} component="span" wrap="nowrap">
          <IconBug size={14} />
          {issueKey ?? url}
          <IconExternalLink size={12} />
        </Group>
      </Anchor>
      {issue && <IssueStateBadge issue={issue} />}
      {issue?.unmuteSuggested && (
        <Text size="xs" c="dimmed" data-testid="defect-unmute-hint">
          Closed while tests are muted:{' '}
          <Anchor component={Link} to={`/p/${encodeURIComponent(project)}/issues?view=suggested`} size="xs">
            review
          </Anchor>
        </Text>
      )}
    </Group>
  );
}
