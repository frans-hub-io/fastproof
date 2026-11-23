import { Alert, Anchor, Badge, Button, Card, Group, Progress, SimpleGrid, Stack, Table, Tabs, Text, Title, Tooltip, UnstyledButton } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { useMemo, useState } from 'react';
import { IconBellOff, IconBug, IconExternalLink, IconHistory, IconLink, IconListCheck, IconTag, IconTopologyStar3 } from '@tabler/icons-react';
import { Link, useNavigate, useParams, useSearchParams } from 'react-router';
import { useLaunch, useProject, useResult } from '../api/hooks';
import { ApiError } from '../api/client';
import { useDefectTypeMap, useMockInteractions, useSimilar } from '../api/stageC';
import type { Attempt, Label, Launch, ResultDetail, SeriesHistoryItem } from '../api/types';
import type { DefectType } from '../api/types-next';
import { DefectChip } from '../components/Defect';
import { DefectIssue } from '../components/DefectIssue';
import { LogConsole } from '../components/LogConsole';
import { TriageDrawer } from '../components/TriageDrawer';
import { resultTarget, type TriageTarget } from '../lib/triage';
import { TransitionBadge } from './launch/TransitionBadge';
import { AnsiText } from '../components/Ansi';
import { stripAnsi } from '../lib/ansi';
import { CopyValue } from '../components/CopyButton';
import { ErrorBlock } from '../components/ErrorBlock';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { StatusBadge } from '../components/Status';
import { StepTree } from '../components/StepTree';
import { formatDateTime, formatDuration, formatRelative, isHttpUrl } from '../lib/format';
import { renderSafeHtml } from '../lib/safeHtml';
import { STATUS_LABEL, statusVar } from '../lib/status';
import { AttachmentList } from '../viewers/AttachmentList';
import { AttemptVideo } from '../viewers/player/AttemptVideo';
import { videosOf } from '../viewers/player/videos';

export function HistoryStrip({ project, history, currentLaunch, fullName }: { project: string; history: SeriesHistoryItem[]; currentLaunch: string | undefined; fullName: string }) {
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

function failing(status: string): boolean {
  return status === 'failed' || status === 'broken';
}

export function DefectCard({ project, r, types }: { project: string; r: ResultDetail; types: Map<string, DefectType> }) {
  const canTriage = useCan('results:triage', project);
  const [target, setTarget] = useState<TriageTarget | null>(null);
  const d = r.defect;
  if (!d && !r.muted && !(canTriage && failing(r.status))) return null;
  return (
    <Card withBorder radius="md" padding="sm" data-testid="defect-card">
      <Group gap="sm" wrap="wrap" justify="space-between" align="flex-start">
        <Stack gap={6} style={{ minWidth: 0, flex: '1 1 260px' }}>
          <Group gap="xs" wrap="wrap">
            {d ? <DefectChip defect={d} types={types} size="md" /> : <Text size="sm" c="dimmed">Not triaged</Text>}
            {r.muted && (
              <Badge variant="outline" color="gray" leftSection={<IconBellOff size={12} />} style={{ textTransform: 'none' }} data-testid="muted-badge">
                {r.muteUntil ? `muted until ${formatDateTime(r.muteUntil)}` : 'muted'}
              </Badge>
            )}
            {r.muted && r.muteReason && (
              <Text size="xs" c="dimmed" style={{ wordBreak: 'break-word' }} data-testid="mute-reason">
                {r.muteReason}
              </Text>
            )}
            {d?.source === 'rule' && d.by && (
              <Text size="xs" c="dimmed">
                set by {d.by.replace(/^rule:/, 'rule ')}
              </Text>
            )}
            {d?.source === 'user' && d.by && (
              <Text size="xs" c="dimmed">
                by {d.by}
                {d.at ? `, ${formatRelative(d.at)}` : ''}
              </Text>
            )}
          </Group>
          {d?.comment && (
            <Text size="sm" style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-word' }} data-testid="defect-comment">
              {d.comment}
            </Text>
          )}
          {isHttpUrl(d?.issueUrl) && <DefectIssue project={project} url={d.issueUrl} issueKey={d.issueKey} />}
        </Stack>
        {canTriage && (
          <Button size="xs" variant="light" leftSection={<IconTag size={14} />} onClick={() => setTarget(resultTarget(r))} data-testid="triage-open">
            Triage
          </Button>
        )}
      </Group>
      <TriageDrawer project={project} target={target} onClose={() => setTarget(null)} />
    </Card>
  );
}

function SimilarFailures({ project, r, types }: { project: string; r: ResultDetail; types: Map<string, DefectType> }) {
  const q = useSimilar(project, r.id, 8);
  if (q.isError && q.error instanceof ApiError && q.error.status === 404) return null;
  return (
    <Card withBorder radius="md" padding="sm" data-testid="similar">
      <Group gap={6} mb={6}>
        <IconTopologyStar3 size={16} />
        <Text size="sm" fw={600}>
          Similar failures
        </Text>
      </Group>
      <QueryState query={q}>
        {() =>
          q.data?.length ? (
            <Stack gap={8}>
              {q.data.map((s) => (
                <UnstyledButton key={s.resultId} component={Link} to={`/p/${project}/results/${s.resultId}`} className="fp-tree-row" data-testid="similar-item">
                  <Stack gap={3}>
                    <Group gap={6} wrap="nowrap">
                      <Text size="xs" c="dimmed" style={{ flex: 'none' }}>
                        #{s.launchNumber}
                      </Text>
                      <Text size="sm" fw={600} className="fh-truncate" style={{ flex: 1, minWidth: 0 }}>
                        {s.name}
                      </Text>
                      <Tooltip label={s.score >= 0.99 ? 'Same fingerprint' : 'Similar message'}>
                        <Text size="xs" c="dimmed" style={{ flex: 'none' }}>
                          {Math.round(s.score * 100)}%
                        </Text>
                      </Tooltip>
                    </Group>
                    <Progress value={s.score * 100} size={3} color={s.score >= 0.99 ? 'fhPurple' : 'gray'} aria-hidden />
                    <Text size="xs" className="fp-mono" c="dimmed" lineClamp={2} style={{ wordBreak: 'break-word' }}>
                      {s.message ? stripAnsi(s.message) : s.message}
                    </Text>
                    {s.defect && (
                      <Group gap={4}>
                        <DefectChip defect={s.defect} types={types} size="xs" />
                      </Group>
                    )}
                  </Stack>
                </UnstyledButton>
              ))}
            </Stack>
          ) : (
            <Text size="sm" c="dimmed">
              No similar failures in this project.
            </Text>
          )
        }
      </QueryState>
    </Card>
  );
}

function MockInteractionsView({ project, resultId, attempt }: { project: string; resultId: string; attempt: number }) {
  const q = useMockInteractions(project, resultId, attempt, true);
  if (q.isError && q.error instanceof ApiError && (q.error.status === 404 || q.error.status === 409)) {
    return (
      <Text size="sm" c="dimmed">
        {q.error.status === 409 ? 'FastMock is not linked to this project. Set its URL in the project settings.' : 'No mock interactions for this attempt.'}
      </Text>
    );
  }
  return (
    <QueryState query={q}>
      {() => {
        const m = q.data;
        if (!m) return null;
        return (
          <Stack gap="xs" data-testid="mock-interactions">
            <Text size="xs" c="dimmed">
              {m.source === 'traceId' ? 'Matched by trace id' : 'Matched by the attempt time window (other tests may share it)'} · confidence {Math.round(m.confidence * 100)}%
            </Text>
            {m.truncated && (
              <Text size="xs" c="dimmed" data-testid="mock-truncated">
                FastMock holds more requests for this window than were read; the list may be incomplete.
              </Text>
            )}
            {m.items.length === 0 ? (
              <Text size="sm" c="dimmed">
                FastMock recorded no requests for this attempt.
              </Text>
            ) : (
              <div className="fp-table-wrap">
                <Table fz="xs" verticalSpacing={4} striped highlightOnHover miw={560}>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th>Time</Table.Th>
                      <Table.Th>Request</Table.Th>
                      <Table.Th>Status</Table.Th>
                      <Table.Th>Stub</Table.Th>
                      <Table.Th ta="right">Took</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {m.items.map((i, n) => (
                      <Table.Tr key={`${i.at}:${String(n)}`} data-testid="mock-row">
                        <Table.Td className="fp-mono">{new Date(i.at).toLocaleTimeString()}</Table.Td>
                        <Table.Td className="fp-mono" style={{ wordBreak: 'break-all' }}>
                          {i.method} {i.url}
                        </Table.Td>
                        <Table.Td>
                          <Badge size="xs" variant="light" color={i.status >= 500 ? 'red' : i.status >= 400 ? 'orange' : 'green'}>
                            {i.status}
                          </Badge>
                        </Table.Td>
                        <Table.Td>
                          {i.matchedStub ? <Text size="xs" className="fp-mono">{i.matchedStub}</Text> : <Text size="xs" c="dimmed">no match</Text>}
                          {i.fault && (
                            <Text size="xs" c="var(--fp-status-broken)">
                              fault: {i.fault}
                            </Text>
                          )}
                          {i.nearMiss && (
                            <Text size="xs" c="var(--fh-semantic-warning-fg)">
                              near miss: see the closest stubs in FastMock
                            </Text>
                          )}
                        </Table.Td>
                        <Table.Td ta="right">{formatDuration(i.durationMs)}</Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </div>
            )}
          </Stack>
        );
      }}
    </QueryState>
  );
}

export function Side({ project, r, types }: { project: string; r: ResultDetail; types: Map<string, DefectType> }) {
  return (
    <Stack gap="md">
      {failing(r.status) && <SimilarFailures project={project} r={r} types={types} />}
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
            {r.links.map((l, i) => {
              const icon = l.type === 'issue' ? <IconBug size={14} /> : l.type === 'tms' ? <IconListCheck size={14} /> : <IconLink size={14} />;
              return isHttpUrl(l.url) ? (
                <Anchor key={`${l.url}:${String(i)}`} href={l.url} target="_blank" rel="noopener noreferrer nofollow" size="sm">
                  <Group gap={6} wrap="nowrap">
                    {icon}
                    <span className="fh-truncate">{l.name || l.url}</span>
                    <IconExternalLink size={12} />
                  </Group>
                </Anchor>
              ) : (
                <Group key={`text:${String(i)}`} gap={6} wrap="nowrap" c="dimmed">
                  {icon}
                  <Text size="sm" className="fh-truncate" title="Not a web address">
                    {l.name || l.url}
                  </Text>
                </Group>
              );
            })}
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

function AttemptView({ project, attempt, r, launch }: { project: string; attempt: Attempt; r: ResultDetail; launch: Launch | undefined }) {
  const start = attempt.startedAt ? new Date(attempt.startedAt).getTime() : null;
  const timeline = start !== null && attempt.durationMs ? { start, total: attempt.durationMs } : null;
  const [sp, setSp] = useSearchParams();
  const section = sp.get('section') ?? 'steps';
  const hasVideo = useMemo(() => videosOf(attempt.attachments).length > 0, [attempt.attachments]);
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
          {hasVideo && (
            <Tabs.Tab value="video" data-testid="tab-result-video">
              Video
            </Tabs.Tab>
          )}
          <Tabs.Tab value="logs" data-testid="tab-result-logs">
            {launch?.status === 'running' ? 'Live logs' : 'Logs'}
          </Tabs.Tab>
          <Tabs.Tab value="output" disabled={!attempt.stdout && !attempt.stderr}>
            Output
          </Tabs.Tab>
          <Tabs.Tab value="mocks">Mock interactions</Tabs.Tab>
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
        {hasVideo && (
          <Tabs.Panel value="video" pt="sm">
            <AttemptVideo project={project} attempt={attempt} />
          </Tabs.Panel>
        )}
        <Tabs.Panel value="logs" pt="sm">
          {r.launchId ? (
            <LogConsole project={project} launchId={r.launchId} live={launch?.status === 'running'} resultId={attempt.clientId ?? r.id} attempt={attempt.clientId ? undefined : attempt.no - 1} height="min(55vh, 520px)" fileName={`${r.name}-attempt-${String(attempt.no)}.log`} />
          ) : (
            <Alert color="gray">The launch of this result is unknown, so its logs cannot be read.</Alert>
          )}
        </Tabs.Panel>
        <Tabs.Panel value="mocks" pt="sm">
          <MockInteractionsView project={project} resultId={r.id} attempt={attempt.no} />
        </Tabs.Panel>
        <Tabs.Panel value="output" pt="sm">
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

export function Attempts({ project, r, launch }: { project: string; r: ResultDetail; launch: Launch | undefined }) {
  const [sp, setSp] = useSearchParams();
  const last = r.attempts[r.attempts.length - 1];
  if (!last) return <Text c="dimmed">No attempts were reported.</Text>;
  if (r.attempts.length === 1) return <AttemptView project={project} attempt={last} r={r} launch={launch} />;
  const asked = sp.get('attempt');
  const current = asked && r.attempts.some((a) => String(a.no) === asked) ? asked : String(last.no);
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
          <AttemptView project={project} attempt={a} r={r} launch={launch} />
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
  const types = useDefectTypeMap(key);

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
                  <TransitionBadge transition={r.transition} />
                  {r.muted && (
                    <Tooltip label={[r.muteUntil ? `Until ${formatDateTime(r.muteUntil)}` : 'Until unmuted', r.muteReason].filter(Boolean).join(' · ')}>
                      <Badge variant="outline" color="gray">
                        muted
                      </Badge>
                    </Tooltip>
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
              <DefectCard project={key} r={r} types={types} />
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
                    <Attempts project={key} r={r} launch={launch.data} />
                  </Card>
                </Stack>
                <Side project={key} r={r} types={types} />
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
