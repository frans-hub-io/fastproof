import { Accordion, ActionIcon, Anchor, Badge, Button, Group, SegmentedControl, Stack, Text, Tooltip } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { IconSparkles, IconTag } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { Link } from 'react-router';
import { ApiError } from '../../api/client';
import { flatItems, useResults } from '../../api/hooks';
import { useClusters, useDefectTypeMap } from '../../api/stageC';
import type { Launch } from '../../api/types';
import type { Cluster, DefectType } from '../../api/types-next';
import { DefectChip } from '../../components/Defect';
import { Loading, QueryState } from '../../components/QueryState';
import { StatusDot } from '../../components/Status';
import { TriageDrawer } from '../../components/TriageDrawer';
import { resultTarget, type TriageTarget } from '../../lib/triage';
import { groupFailures } from '../../lib/errorGroups';
import { formatDate } from '../../lib/format';
import { TransitionBadge } from './TransitionBadge';

function ClusterResults({ project, launch, cluster, types, canTriage, onTriage }: { project: string; launch: Launch; cluster: Cluster; types: Map<string, DefectType>; canTriage: boolean; onTriage: (t: TriageTarget) => void }) {
  const q = useResults(project, launch.id, { fingerprint: cluster.fingerprint, status: 'failed,broken' });
  const items = flatItems(q.data);
  return (
    <QueryState query={q}>
      {() => (
        <Stack gap={0} data-testid="cluster-results">
          {items.map((r) => (
            <Group key={r.id} gap="xs" wrap="wrap" py={4} style={{ borderBottom: '1px solid var(--fh-ui-hairline)' }}>
              <Group gap="xs" wrap="nowrap" style={{ flex: '1 1 260px', minWidth: 0 }}>
                <StatusDot status={r.status} />
                <Anchor component={Link} to={`/p/${project}/results/${r.id}`} size="sm" c="inherit" className="fh-truncate" style={{ minWidth: 0 }}>
                  {r.fullName}
                </Anchor>
              </Group>
              <TransitionBadge transition={r.transition} />
              {r.muted && (
                <Badge size="xs" variant="outline" color="gray" style={{ flex: 'none' }}>
                  muted
                </Badge>
              )}
              {r.defect && <DefectChip defect={r.defect} types={types} size="xs" />}
              {canTriage && (
                <Tooltip label="Triage this result">
                  <ActionIcon variant="subtle" size="sm" aria-label={`Triage ${r.name}`} onClick={() => onTriage(resultTarget(r))} data-testid="triage-result">
                    <IconTag size={14} />
                  </ActionIcon>
                </Tooltip>
              )}
            </Group>
          ))}
        </Stack>
      )}
    </QueryState>
  );
}

function MessageGroups({ project, launch }: { project: string; launch: Launch }) {
  const q = useResults(project, launch.id, { status: 'failed,broken' });
  const groups = useMemo(() => groupFailures(flatItems(q.data)), [q.data]);
  if (q.isPending) return <Loading />;
  if (!groups.length) return <EmptyState title="No failures" description="Every executed test passed in this launch." />;
  return (
    <Stack gap="xs">
      <Text size="sm" c="dimmed">
        {groups.length} groups by error message. This server does not cluster failures yet.
      </Text>
      <Accordion variant="separated" radius="md" multiple data-testid="failure-groups">
        {groups.map((g) => (
          <Accordion.Item key={g.key} value={g.key} data-testid="failure-group">
            <Accordion.Control>
              <Group gap="sm" wrap="nowrap">
                <Badge variant="filled" color="red" radius="sm" style={{ flex: 'none' }}>
                  {g.results.length}
                </Badge>
                <Text size="sm" className="fp-mono" style={{ wordBreak: 'break-word' }}>
                  {g.title}
                </Text>
              </Group>
            </Accordion.Control>
            <Accordion.Panel>
              {g.results.map((r) => (
                <Anchor key={r.id} component={Link} to={`/p/${project}/results/${r.id}`} size="sm" py={4} c="inherit" display="block">
                  <Group gap="xs" wrap="nowrap">
                    <StatusDot status={r.status} />
                    <Text size="sm" className="fh-truncate">
                      {r.fullName}
                    </Text>
                  </Group>
                </Anchor>
              ))}
            </Accordion.Panel>
          </Accordion.Item>
        ))}
      </Accordion>
    </Stack>
  );
}

export function Clusters({ project, launch }: { project: string; launch: Launch }) {
  const q = useClusters(project, launch.id);
  const types = useDefectTypeMap(project);
  const canTriage = useCan('results:triage', project);
  const [show, setShow] = useState<string>('all');
  const [open, setOpen] = useState<string[]>([]);
  const [target, setTarget] = useState<TriageTarget | null>(null);

  if (q.isError && q.error instanceof ApiError && (q.error.status === 404 || q.error.status === 405)) return <MessageGroups project={project} launch={launch} />;

  return (
    <QueryState query={q}>
      {() => {
        const all = q.data ?? [];
        if (!all.length) return <EmptyState title="No failures" description="Every executed test passed in this launch." />;
        const fresh = all.filter((c) => c.isNew).length;
        const list = all.filter((c) => show === 'all' || (show === 'new' ? c.isNew : !c.isNew));
        return (
          <Stack gap="sm">
            <Group justify="space-between" gap="sm" wrap="wrap">
              <Text size="sm" c="dimmed" data-testid="cluster-summary">
                {all.length} cluster{all.length === 1 ? '' : 's'} by failure fingerprint · {fresh} new in this launch
              </Text>
              <SegmentedControl
                size="xs"
                value={show}
                onChange={(v) => setShow(v)}
                data={[
                  { value: 'all', label: `All ${String(all.length)}` },
                  { value: 'new', label: `New ${String(fresh)}` },
                  { value: 'known', label: `Known ${String(all.length - fresh)}` },
                ]}
              />
            </Group>
            <Accordion variant="separated" radius="md" multiple value={open} onChange={setOpen} data-testid="clusters">
              {list.map((c) => (
                <Accordion.Item key={c.fingerprint} value={c.fingerprint} data-testid="cluster" data-new={c.isNew}>
                  <Accordion.Control>
                    <Stack gap={6}>
                      <Group gap="sm" wrap="nowrap" align="flex-start">
                        <Badge variant="filled" color={c.statuses.failed >= c.statuses.broken ? 'red' : 'orange'} radius="sm" style={{ flex: 'none' }} data-testid="cluster-count">
                          {c.count}
                        </Badge>
                        <Text size="sm" className="fp-mono" lineClamp={2} style={{ wordBreak: 'break-word', minWidth: 0 }}>
                          {c.message || '(no message)'}
                        </Text>
                      </Group>
                      <Group gap="xs" wrap="wrap">
                        {c.isNew ? (
                          <Badge size="xs" color="yellow" variant="light" leftSection={<IconSparkles size={11} />} style={{ textTransform: 'none' }}>
                            New
                          </Badge>
                        ) : (
                          <Badge size="xs" color="gray" variant="light" style={{ textTransform: 'none' }}>
                            Known since {c.knownSince ? formatDate(c.knownSince) : 'earlier'}
                          </Badge>
                        )}
                        {c.defect && <DefectChip defect={c.defect} types={types} size="xs" />}
                        <Text size="xs" c="dimmed">
                          {c.statuses.failed} failed · {c.statuses.broken} broken
                        </Text>
                        <Text size="xs" c="dimmed" className="fp-mono">
                          {c.fingerprint}
                        </Text>
                      </Group>
                    </Stack>
                  </Accordion.Control>
                  <Accordion.Panel>
                    <Stack gap="xs">
                      {canTriage && (
                        <Group justify="flex-end">
                          <Button
                            size="xs"
                            variant="light"
                            leftSection={<IconTag size={14} />}
                            onClick={() => setTarget({ label: `${String(c.count)} results with: ${c.message}`, count: c.count, launchId: launch.id, fingerprint: c.fingerprint, defect: c.defect })}
                            data-testid="triage-cluster"
                          >
                            Triage all {c.count}
                          </Button>
                        </Group>
                      )}
                      {open.includes(c.fingerprint) && <ClusterResults project={project} launch={launch} cluster={c} types={types} canTriage={canTriage} onTriage={setTarget} />}
                    </Stack>
                  </Accordion.Panel>
                </Accordion.Item>
              ))}
            </Accordion>
            <TriageDrawer project={project} target={target} onClose={() => setTarget(null)} />
          </Stack>
        );
      }}
    </QueryState>
  );
}
