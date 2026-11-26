import { Anchor, Badge, Text } from '@mantine/core';
import { IconExternalLink } from '@tabler/icons-react';
import { Link } from 'react-router';
import type { LastResult, RequirementLinkOrigin, RequirementStatus } from '../../api/requirements';
import { requirementKey } from '../../api/requirements';
import { StatusBadge } from '../../components/Status';
import { isHttpUrl } from '../../lib/format';
import type { ResultStatus } from '../../api/types';

const STATUS_COLOR: Record<RequirementStatus, string> = { draft: 'gray', active: 'blue', done: 'green', obsolete: 'orange' };

export function RequirementStatusBadge({ status }: { status: RequirementStatus }) {
  return (
    <Badge size="sm" variant="light" radius="sm" color={STATUS_COLOR[status]} data-requirement-status={status} style={{ textTransform: 'none' }}>
      {status}
    </Badge>
  );
}

export function RequirementLinkText({ project, number, title }: { project: string; number: number; title?: string }) {
  return (
    <Anchor component={Link} to={`/p/${project}/requirements/${String(number)}`} size="sm" data-testid="requirement-link">
      <span className="fp-mono">{requirementKey(number)}</span>
      {title ? ` ${title}` : ''}
    </Anchor>
  );
}

const ORIGIN_LABEL: Record<RequirementLinkOrigin, string> = { ui: 'Manual', code: 'From code', import: 'Imported', ai: 'AI proposal' };

export function OriginBadge({ origin }: { origin: RequirementLinkOrigin }) {
  return (
    <Badge size="xs" variant="outline" color="gray" radius="sm" style={{ textTransform: 'none' }}>
      {ORIGIN_LABEL[origin]}
    </Badge>
  );
}

export function ProposedBadge() {
  return (
    <Badge size="xs" variant="light" color="grape" radius="sm" data-testid="proposed-badge" style={{ textTransform: 'none' }}>
      Proposed
    </Badge>
  );
}

const RESULT_STATUSES = new Set(['passed', 'failed', 'broken', 'skipped', 'unknown']);

export function LastResultChip({ result }: { result?: LastResult | null }) {
  if (!result) {
    return (
      <Text size="xs" c="dimmed">
        Not run
      </Text>
    );
  }
  return RESULT_STATUSES.has(result.status) ? <StatusBadge status={result.status as ResultStatus} size="xs" /> : <Badge size="xs" variant="light" radius="sm">{result.status}</Badge>;
}

export function IssueLink({ issueKey, url, externalState }: { issueKey?: string | null; url?: string | null; externalState?: string | null }) {
  if (!issueKey) return <Text size="sm" c="dimmed">—</Text>;
  return (
    <Text size="sm" component="span" style={{ overflowWrap: 'anywhere' }}>
      {isHttpUrl(url) ? (
        <Anchor href={url} target="_blank" rel="noopener noreferrer nofollow" size="sm">
          {issueKey} <IconExternalLink size={12} aria-hidden style={{ verticalAlign: -1 }} />
        </Anchor>
      ) : (
        issueKey
      )}
      {externalState && (
        <Badge size="xs" variant="outline" color="gray" radius="sm" ml={6} style={{ textTransform: 'none' }}>
          {externalState}
        </Badge>
      )}
    </Text>
  );
}
