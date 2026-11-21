import { ActionIcon, Anchor, Badge, Card, Group, Stack, Text, Title, Tooltip } from '@mantine/core';
import { IconBellOff, IconExternalLink, IconRepeat, IconX } from '@tabler/icons-react';
import { Link } from 'react-router';
import { useResult } from '../../api/hooks';
import { useDefectTypeMap } from '../../api/stageC';
import type { Launch } from '../../api/types';
import { QueryState } from '../../components/QueryState';
import { StatusBadge } from '../../components/Status';
import { formatDateTime, formatDuration } from '../../lib/format';
import { renderSafeHtml } from '../../lib/safeHtml';
import { Attempts, DefectCard, HistoryStrip, Side } from '../Result';
import { TransitionBadge } from './TransitionBadge';

/** The selected test of the explorer, beside the tree. */
export function ExplorerDetail({ project, launch, resultId, onClose }: { project: string; launch: Launch; resultId: string; onClose?: () => void }) {
  const q = useResult(project, resultId);
  const types = useDefectTypeMap(project);
  return (
    <QueryState query={q} notFound="This result is not in the project">
      {() => {
        const r = q.data;
        if (!r) return null;
        return (
          <Stack gap="md" data-testid="explorer-detail" data-result={r.id}>
            <Group gap="xs" wrap="nowrap" align="flex-start" justify="space-between">
              <Stack gap={4} style={{ minWidth: 0, flex: 1 }}>
                <Group gap={6} wrap="wrap">
                  <StatusBadge status={r.status} size="md" />
                  {r.flakyInRun && <StatusBadge status="flaky" size="md" />}
                  <TransitionBadge transition={r.transition} />
                  {r.muted && (
                    <Tooltip label={[r.muteUntil ? `Until ${formatDateTime(r.muteUntil)}` : 'Until unmuted', r.muteReason].filter(Boolean).join(' · ')}>
                      <Badge variant="outline" color="gray" leftSection={<IconBellOff size={12} />} style={{ textTransform: 'none' }}>
                        muted
                      </Badge>
                    </Tooltip>
                  )}
                  {r.attemptCount > 1 && (
                    <Badge variant="light" color="gray" leftSection={<IconRepeat size={12} />} style={{ textTransform: 'none' }}>
                      {r.attemptCount} attempts
                    </Badge>
                  )}
                </Group>
                <Title order={3} style={{ wordBreak: 'break-word' }} data-testid="explorer-detail-name">
                  {r.name}
                </Title>
                <Text size="xs" className="fp-mono" c="dimmed" style={{ wordBreak: 'break-all' }}>
                  {r.fullName}
                </Text>
                <Group gap="md" wrap="wrap">
                  <Text size="sm" c="dimmed">
                    {formatDuration(r.durationMs)}
                  </Text>
                  <Anchor component={Link} to={`/p/${project}/test-cases/${encodeURIComponent(r.testCaseId)}`} size="sm">
                    Test case history
                  </Anchor>
                  <Anchor component={Link} to={`/p/${project}/results/${r.id}`} size="sm" data-testid="explorer-open-full">
                    <Group gap={4} component="span" wrap="nowrap">
                      Open full page <IconExternalLink size={12} />
                    </Group>
                  </Anchor>
                </Group>
              </Stack>
              {onClose && (
                <Tooltip label="Close (Esc)">
                  <ActionIcon variant="subtle" color="gray" onClick={onClose} aria-label="Close the test details" data-testid="explorer-detail-close">
                    <IconX size={18} />
                  </ActionIcon>
                </Tooltip>
              )}
            </Group>
            <HistoryStrip project={project} history={r.history} currentLaunch={r.launchId} fullName={r.fullName} />
            <DefectCard project={project} r={r} types={types} />
            {r.description && (
              <Card withBorder radius="md" padding="sm">
                <Text size="sm" fw={600} mb={6}>
                  Description
                </Text>
                <div className="fp-description">{renderSafeHtml(r.description)}</div>
              </Card>
            )}
            <Card withBorder radius="md" padding="md">
              <Attempts project={project} r={r} launch={launch} />
            </Card>
            <Side project={project} r={r} types={types} />
          </Stack>
        );
      }}
    </QueryState>
  );
}
