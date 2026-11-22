import { Anchor, Card, Group, SegmentedControl, SimpleGrid, Stack, Text, TextInput, Tooltip } from '@mantine/core';
import { EmptyState } from '@franshub/ui-shell';
import { useDebouncedValue } from '@mantine/hooks';
import { IconRobot } from '@tabler/icons-react';
import { useState } from 'react';
import { Link } from 'react-router';
import { useCaseHistory, type CaseHistoryEntry } from '../../api/tm';
import { QueryState } from '../../components/QueryState';
import { StatusBadge } from '../../components/Status';
import type { ResultStatus } from '../../api/types';
import { formatDateTime, formatRelative } from '../../lib/format';
import { STATUS_LABEL, statusVar } from '../../lib/status';
import { ManualIcon } from './common';

function asStatus(s: string): ResultStatus {
  return s === 'passed' || s === 'failed' || s === 'broken' || s === 'skipped' ? s : 'unknown';
}

function Strip({ items }: { items: CaseHistoryEntry[] }) {
  const last = items.slice(0, 40).reverse();
  return (
    <div className="fp-history" role="img" aria-label={`Last ${String(last.length)} results: ${last.map((i) => STATUS_LABEL[asStatus(i.status)]).join(', ')}`}>
      {last.map((i) => (
        <Tooltip key={`${i.resultId}:${i.source}`} label={`${STATUS_LABEL[asStatus(i.status)]} · ${i.source} · #${String(i.launchNumber)} · ${formatRelative(i.at)}`}>
          <span className="fp-history-cell" style={{ background: statusVar(i.status) }} />
        </Tooltip>
      ))}
    </div>
  );
}

function resultLink(project: string, i: CaseHistoryEntry): string {
  return i.source === 'manual' ? `/p/${project}/launches/${i.launchId}?tab=results&source=manual` : `/p/${project}/results/${i.resultId}`;
}

export function HistoryTab({ project, n }: { project: string; n: number }) {
  const [source, setSource] = useState<'all' | 'manual' | 'automated'>('all');
  const [branch, setBranch] = useState('');
  const [environment, setEnvironment] = useState('');
  const [db] = useDebouncedValue(branch, 300);
  const [de] = useDebouncedValue(environment, 300);
  const q = useCaseHistory(project, n, { source: source === 'all' ? undefined : source, branch: db || undefined, environment: de || undefined });
  const items = q.data ?? [];
  return (
    <Stack gap="sm" data-testid="history-tab">
      <Group gap="xs" wrap="wrap">
        <SegmentedControl
          size="xs"
          value={source}
          onChange={(v) => setSource(v === 'manual' || v === 'automated' ? v : 'all')}
          data={[
            { value: 'all', label: 'All' },
            { value: 'manual', label: 'Manual' },
            { value: 'automated', label: 'Automated' },
          ]}
          aria-label="Result source"
        />
        <SimpleGrid cols={2} spacing="xs" style={{ flex: '1 1 300px' }}>
          <TextInput size="xs" aria-label="Branch" placeholder="Branch" value={branch} onChange={(e) => setBranch(e.currentTarget.value)} />
          <TextInput size="xs" aria-label="Environment" placeholder="Environment" value={environment} onChange={(e) => setEnvironment(e.currentTarget.value)} />
        </SimpleGrid>
      </Group>
      <QueryState query={q}>
        {() =>
          items.length === 0 ? (
            <EmptyState title="No results yet" description="Results appear when the case runs, manually or in CI." />
          ) : (
            <>
              <Strip items={items} />
              <Card withBorder radius="md" padding={0}>
                {items.map((i) => (
                  <Group key={`${i.resultId}:${i.source}`} gap="sm" wrap="nowrap" p="xs" style={{ borderBottom: '1px solid var(--fh-ui-hairline)' }} data-testid="history-row" data-source={i.source}>
                    <div style={{ width: 80, flex: 'none' }}>
                      <StatusBadge status={asStatus(i.status)} size="xs" />
                    </div>
                    {i.source === 'manual' ? (
                      <ManualIcon />
                    ) : (
                      <span role="img" aria-label="Automated result" style={{ display: 'inline-flex', color: 'var(--mantine-color-dimmed)' }}>
                        <IconRobot size={14} aria-hidden />
                      </span>
                    )}
                    <Stack gap={0} style={{ minWidth: 0, flex: 1 }}>
                      <Anchor component={Link} to={resultLink(project, i)} size="sm" className="fh-truncate">
                        #{i.launchNumber} {i.launchName}
                      </Anchor>
                      <Text size="xs" c="dimmed" className="fh-truncate">
                        {[i.branch, i.environment, i.executedBy].filter(Boolean).join(' · ')}
                      </Text>
                    </Stack>
                    <Text size="xs" c="dimmed" title={formatDateTime(i.at)} style={{ flex: 'none' }}>
                      {formatRelative(i.at)}
                    </Text>
                  </Group>
                ))}
              </Card>
            </>
          )
        }
      </QueryState>
    </Stack>
  );
}
