import { Anchor, Badge, Card, Group, SimpleGrid, Stack, Table, Tabs, Text, Title, Tooltip, UnstyledButton } from '@mantine/core';
import { IconBug, IconExternalLink, IconHistory, IconLink, IconListCheck } from '@tabler/icons-react';
import { Link, useNavigate, useParams, useSearchParams } from 'react-router';
import { useLaunch, useProject, useResult } from '../api/hooks';
import type { Attempt, Label, ResultDetail, SeriesHistoryItem } from '../api/types';
import { AnsiText } from '../components/Ansi';
import { CopyValue } from '../components/CopyButton';
import { ErrorBlock } from '../components/ErrorBlock';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { StatusBadge } from '../components/Status';
import { StepTree } from '../components/StepTree';
import { formatDateTime, formatDuration, isHttpUrl } from '../lib/format';
import { renderSafeHtml } from '../lib/safeHtml';
import { STATUS_LABEL, statusVar } from '../lib/status';
import { AttachmentList } from '../viewers/AttachmentList';

function HistoryStrip({ project, history, currentLaunch, fullName }: { project: string; history: SeriesHistoryItem[]; currentLaunch: string | undefined; fullName: string }) {
  const navigate = useNavigate();
  if (!history.length) return null;
  const ordered = [...history].reverse();
  return (
    <Stack gap={6}>
      <Group gap="xs">
        <IconHistory size={16} />
        <Text size="sm" fw={600}>
          History of this series
        </Text>
        <Text size="xs" c="dimmed">
          last {history.length}, oldest first
        </Text>
      </Group>
      <div className="fp-history" data-testid="history-strip">
        {ordered.map((h) => (
          <Tooltip key={h.launchId} label={`#${String(h.launchNumber)} · ${STATUS_LABEL[h.status]} · ${formatDateTime(h.startedAt)}`}>
            <button
              type="button"
              className="fp-history-cell"
              style={{ background: statusVar(h.status) }}
              data-current={h.launchId === currentLaunch}
              data-status={h.status}
              aria-label={`Launch #${String(h.launchNumber)}: ${h.status}`}
              onClick={() =>
                void navigate(h.resultId ? `/p/${project}/results/${h.resultId}` : `/p/${project}/launches/${h.launchId}?q=${encodeURIComponent(fullName)}`)
              }
            />
          </Tooltip>
        ))}
      </div>
    </Stack>
  );
}

function Labels({ labels }: { labels: Label[] }) {
  if (!labels.length) return null;
  return (
    <Group gap={6} wrap="wrap" data-testid="labels">
      {labels.map((l) => (
        <Badge key={`${l.name}=${l.value}`} variant="default" radius="sm" style={{ textTransform: 'none', fontWeight: 500 }}>
          <Text span size="xs" c="dimmed">
            {l.name}:
          </Text>{' '}
          {l.value}
        </Badge>
      ))}
    </Group>
  );
}

function Side({ r }: { r: ResultDetail }) {
  return (
    <Stack gap="md">
      {r.parameters.length > 0 && (
        <Card withBorder radius="md" padding="sm">
          <Text size="sm" fw={600} mb={6}>
            Parameters
          </Text>
          <Table fz="sm" verticalSpacing={4} data-testid="parameters">
            <Table.Tbody>
              {r.parameters.map((p) => (
                <Table.Tr key={p.name}>
                  <Table.Td c="dimmed" style={{ width: '40%' }}>
                    {p.name}
                  </Table.Td>
                  <Table.Td className="fp-mono" style={{ wordBreak: 'break-all' }}>
                    {p.value}
                  </Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </Card>
      )}
      {r.links.length > 0 && (
        <Card withBorder radius="md" padding="sm">
          <Text size="sm" fw={600} mb={6}>
            Links
          </Text>
          <Stack gap={4} data-testid="links">
            {r.links
              .filter((l) => isHttpUrl(l.url))
              .map((l) => (
                <Anchor key={l.url} href={l.url} target="_blank" rel="noopener noreferrer nofollow" size="sm">
                  <Group gap={6} wrap="nowrap">
                    {l.type === 'issue' ? <IconBug size={14} /> : l.type === 'tms' ? <IconListCheck size={14} /> : <IconLink size={14} />}
                    <span className="fh-truncate">{l.name || l.url}</span>
                    <IconExternalLink size={12} />
                  </Group>
                </Anchor>
              ))}
          </Stack>
        </Card>
      )}
      <Card withBorder radius="md" padding="sm">
        <Text size="sm" fw={600} mb={6}>
          Labels
        </Text>
        {r.labels.length ? <Labels labels={r.labels} /> : <Text size="sm" c="dimmed">No labels</Text>}
      </Card>
      <Card withBorder radius="md" padding="sm">
        <Text size="sm" fw={600} mb={6}>
          Identity
        </Text>
        <Stack gap={4}>
          {(
            [
              ['Test case', r.testCaseId],
              ['Series', r.seriesKey],
              ['Fingerprint', r.fingerprint],
            ] as const
          )
            .filter(([, v]) => !!v)
            .map(([k, v]) => (
              <Group key={k} gap={4} wrap="nowrap">
                <Text size="xs" c="dimmed" style={{ width: 80, flex: 'none' }}>
                  {k}
                </Text>
                <Text size="xs" className="fp-mono fh-truncate" style={{ flex: 1, minWidth: 0 }} title={v ?? ''}>
                  {v}
                </Text>
                <CopyValue value={v ?? ''} label={`Copy ${k.toLowerCase()}`} />
              </Group>
            ))}
        </Stack>
      </Card>
    </Stack>
  );
}

function AttemptView({ project, attempt }: { project: string; attempt: Attempt }) {
  const start = attempt.startedAt ? new Date(attempt.startedAt).getTime() : null;
  const timeline = start !== null && attempt.durationMs ? { start, total: attempt.durationMs } : null;
  const [sp, setSp] = useSearchParams();
  const section = sp.get('section') ?? 'steps';
  return (
    <Stack gap="md" data-testid={`attempt-${String(attempt.no)}`}>
      <Group gap="md" wrap="wrap">
        <StatusBadge status={attempt.status} />
        <Text size="sm" c="dimmed">
          {formatDateTime(attempt.startedAt)} · {formatDuration(attempt.durationMs)}
        </Text>
        {attempt.traceId && (
          <Group gap={4} wrap="nowrap">
            <Text size="xs" c="dimmed">
              trace id
            </Text>
            <Text size="xs" className="fp-mono">
              {attempt.traceId}
            </Text>
            <CopyValue value={attempt.traceId} label="Copy the trace id" />
          </Group>
        )}
      </Group>
      {attempt.error && <ErrorBlock error={attempt.error} />}
      <Tabs
        value={section}
        onChange={(v) =>
          setSp(
            (prev) => {
              const n = new URLSearchParams(prev);
              if (!v || v === 'steps') n.delete('section');
              else n.set('section', v);
              return n;
            },
            { replace: true },
          )
        }
        keepMounted={false}
        variant="pills"
        radius="md"
      >
        <Tabs.List>
          <Tabs.Tab value="steps">Steps</Tabs.Tab>
          <Tabs.Tab value="attachments" rightSection={attempt.attachments.length ? <Badge size="xs" variant="light" color="gray">{attempt.attachments.length}</Badge> : undefined}>
            Attachments
          </Tabs.Tab>
          <Tabs.Tab value="logs" disabled={!attempt.stdout && !attempt.stderr}>
            Logs
          </Tabs.Tab>
        </Tabs.List>
        <Tabs.Panel value="steps" pt="sm">
          <StepTree project={project} steps={attempt.steps} timeline={timeline} />
        </Tabs.Panel>
        <Tabs.Panel value="attachments" pt="sm">
          {attempt.attachments.length ? (
            <AttachmentList project={project} attachments={attempt.attachments} />
          ) : (
            <Text size="sm" c="dimmed">
              No attachments on this attempt. Steps may carry their own.
            </Text>
          )}
        </Tabs.Panel>
        <Tabs.Panel value="logs" pt="sm">
          <Stack gap="sm">
            {attempt.stdout && (
              <Stack gap={4}>
                <Text size="xs" fw={600} c="dimmed">
                  stdout
                </Text>
                <AnsiText text={attempt.stdout} testId="stdout" />
              </Stack>
            )}
            {attempt.stderr && (
              <Stack gap={4}>
                <Text size="xs" fw={600} c="dimmed">
                  stderr
                </Text>
                <AnsiText text={attempt.stderr} testId="stderr" />
              </Stack>
            )}
          </Stack>
        </Tabs.Panel>
      </Tabs>
    </Stack>
  );
}

function Attempts({ project, r }: { project: string; r: ResultDetail }) {
  const [sp, setSp] = useSearchParams();
  const last = r.attempts[r.attempts.length - 1];
  if (!last) return <Text c="dimmed">No attempts were reported.</Text>;
  if (r.attempts.length === 1) return <AttemptView project={project} attempt={last} />;
  const current = sp.get('attempt') ?? String(last.no);
  return (
    <Tabs
      value={current}
      onChange={(v) =>
        setSp(
          (prev) => {
            const n = new URLSearchParams(prev);
            if (v) n.set('attempt', v);
            return n;
          },
          { replace: true },
        )
      }
      keepMounted={false}
    >
      <Tabs.List>
        {r.attempts.map((a) => (
          <Tabs.Tab key={a.no} value={String(a.no)} leftSection={<span style={{ width: 8, height: 8, borderRadius: 4, background: statusVar(a.status), display: 'inline-block' }} />}>
            Attempt {a.no}
          </Tabs.Tab>
        ))}
      </Tabs.List>
      {r.attempts.map((a) => (
        <Tabs.Panel key={a.no} value={String(a.no)} pt="md">
          <AttemptView project={project} attempt={a} />
        </Tabs.Panel>
      ))}
    </Tabs>
  );
}

export default function ResultPage() {
  const { key = '', id = '' } = useParams();
  const project = useProject(key);
  const q = useResult(key, id);
  const launchId = q.data?.launchId ?? q.data?.history.find((h) => h.resultId === id)?.launchId;
  const launch = useLaunch(key, launchId ?? '');
  const launchNumber = q.data?.launchNumber ?? launch.data?.number;

  return (
    <QueryState query={q} notFound="Result not found">
      {() => {
        const r = q.data;
        if (!r) return null;
        const crumbs = [{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }];
        if (launchId) crumbs.push({ label: launchNumber ? `#${String(launchNumber)}` : 'Launch', to: `/p/${key}/launches/${launchId}` });
        crumbs.push({ label: r.name, to: '' });
        return (
          <>
            <PageHeader
              crumbs={crumbs.map((c) => (c.to ? c : { label: c.label }))}
              title={r.name}
              badges={
                <>
                  <StatusBadge status={r.status} size="md" />
                  {r.flakyInRun && <StatusBadge status="flaky" size="md" />}
                  {r.muted && (
                    <Badge variant="outline" color="gray">
                      muted
                    </Badge>
                  )}
                </>
              }
              subtitle={
                <Stack gap={2}>
                  <Text size="sm" className="fp-mono" style={{ wordBreak: 'break-all' }}>
                    {r.fullName}
                  </Text>
                  <Group gap="md">
                    <Text size="sm" c="dimmed">
                      {formatDuration(r.durationMs)} · {r.attemptCount} attempt{r.attemptCount === 1 ? '' : 's'}
                    </Text>
                    <Anchor component={Link} to={`/p/${key}/test-cases/${encodeURIComponent(r.testCaseId)}`} size="sm" data-testid="test-case-link">
                      Test case history
                    </Anchor>
                  </Group>
                </Stack>
              }
            />
            <Stack gap="md">
              <HistoryStrip project={key} history={r.history} currentLaunch={launchId} fullName={r.fullName} />
              {r.defect && (
                <Card withBorder radius="md" padding="sm">
                  <Group gap="sm" wrap="wrap">
                    <Badge color="orange" variant="light" style={{ textTransform: 'none' }}>
                      {r.defect.type.replace(/_/g, ' ')}
                    </Badge>
                    {r.defect.comment && <Text size="sm">{r.defect.comment}</Text>}
                    {isHttpUrl(r.defect.issueUrl) && (
                      <Anchor href={r.defect.issueUrl} target="_blank" rel="noopener noreferrer" size="sm">
                        {r.defect.issueUrl}
                      </Anchor>
                    )}
                  </Group>
                </Card>
              )}
              <SimpleGrid cols={{ base: 1, lg: 3 }} spacing="md">
                <Stack gap="md" className="fp-result-main">
                  {r.description && (
                    <Card withBorder radius="md" padding="sm">
                      <Title order={5} mb={6}>
                        Description
                      </Title>
                      <div className="fp-description" data-testid="description">
                        {renderSafeHtml(r.description)}
                      </div>
                    </Card>
                  )}
                  <Card withBorder radius="md" padding="md">
                    <Attempts project={key} r={r} />
                  </Card>
                </Stack>
                <Side r={r} />
              </SimpleGrid>
              <UnstyledButton component={Link} to={launchId ? `/p/${key}/launches/${launchId}` : `/p/${key}`}>
                <Text size="sm" c="dimmed">
                  ← Back to the launch
                </Text>
              </UnstyledButton>
            </Stack>
          </>
        );
      }}
    </QueryState>
  );
}
