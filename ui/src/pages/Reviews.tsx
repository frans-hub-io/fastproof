import { Alert, Anchor, Button, Card, Checkbox, Group, SegmentedControl, Select, Stack, Table, Tabs, Text, Textarea, TextInput } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconCheck, IconChecks, IconMessageX } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { Link, useParams, useSearchParams } from 'react-router';
import { errorMessage, projectPath, request, withQuery } from '../api/client';
import { flatItems, useProject } from '../api/hooks';
import { SKIP_LABEL, fetchQueryNumbers, fetchWholeQueue, useBulkReview, useCaseWorkflow, useReviewEvents, useReviewQueue, type ReviewBulkResult, type ReviewDecision, type ReviewKind, type ReviewQuery, type ReviewQueueItem } from '../api/reviews';
import { useCaseFolders, useManagedPlans, type TestPlanItem } from '../api/tm';
import { QueryState } from '../components/QueryState';
import { PageHeader } from '../components/PageHeader';
import { formatRelative } from '../lib/format';
import { folderPathLabel } from '../lib/tm/ui';
import { itemKey, itemLink, bytesOf } from '../lib/tm/reviewUtil';
import { StateBadge, ReviewDiff } from './cases/ReviewTab';

type Who = '' | 'reviewer' | 'author';

const STATE_OPTIONS = [
  { value: 'pending', label: 'Waiting for review' },
  { value: 'changes_requested', label: 'Changes requested' },
  { value: 'approved', label: 'Approved' },
];
const KIND_OPTIONS = [
  { value: '', label: 'All kinds' },
  { value: 'case', label: 'Test cases' },
  { value: 'sharedStep', label: 'Shared steps' },
];

function ItemName({ project, item }: { project: string; item: ReviewQueueItem }) {
  return (
    <Anchor component={Link} to={itemLink(project, item)} size="sm" fw={600} style={{ wordBreak: 'break-word' }} data-testid="review-link">
      {item.name}
    </Anchor>
  );
}

function Changed({ item }: { item: ReviewQueueItem }) {
  if (item.changed.length === 0) return <Text size="xs" c="dimmed">—</Text>;
  return (
    <Text size="xs" c="dimmed" className="fh-truncate" title={item.changed.join(', ')}>
      {item.changed.join(', ')}
    </Text>
  );
}

function QueueRows({ project, items, mobile }: { project: string; items: ReviewQueueItem[]; mobile: boolean }) {
  if (mobile) {
    return (
      <Stack gap="xs" role="list" aria-label="Review queue">
        {items.map((i) => (
          <Card key={`${i.kind}-${String(i.number)}`} withBorder radius="md" padding="sm" role="listitem" data-testid="review-card" data-key={itemKey(i)}>
            <Stack gap={4}>
              <Group gap={6} wrap="wrap">
                <Text size="xs" c="dimmed" className="fp-mono">
                  {itemKey(i)}
                </Text>
                <StateBadge state={i.reviewState} />
              </Group>
              <ItemName project={project} item={i} />
              <Text size="xs" c="dimmed">
                v{i.approvedVersion ?? '—'} → v{i.headVersion} · {i.approvals} / {i.required} approvals
              </Text>
              <Text size="xs" c="dimmed">
                {i.author ?? 'unknown'} · {formatRelative(i.updatedAt)}
              </Text>
              <Changed item={i} />
            </Stack>
          </Card>
        ))}
      </Stack>
    );
  }
  return (
    <div className="fp-table-wrap">
      <Table highlightOnHover verticalSpacing="xs" fz="sm" data-testid="reviews-table">
        <Table.Thead>
          <Table.Tr>
            <Table.Th scope="col">Key</Table.Th>
            <Table.Th scope="col">Name</Table.Th>
            <Table.Th scope="col">State</Table.Th>
            <Table.Th scope="col">Approvals</Table.Th>
            <Table.Th scope="col">Author</Table.Th>
            <Table.Th scope="col">Changed</Table.Th>
            <Table.Th scope="col">Updated</Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {items.map((i) => (
            <Table.Tr key={`${i.kind}-${String(i.number)}`} data-testid="review-row" data-key={itemKey(i)}>
              <Table.Td>
                <Text size="xs" className="fp-mono" style={{ whiteSpace: 'nowrap' }}>
                  {itemKey(i)}
                </Text>
              </Table.Td>
              <Table.Td maw={380}>
                <ItemName project={project} item={i} />
              </Table.Td>
              <Table.Td>
                <StateBadge state={i.reviewState} />
              </Table.Td>
              <Table.Td>
                <Text size="sm">
                  {i.approvals} / {i.required}
                </Text>
                <Text size="xs" c="dimmed" className="fp-mono">
                  v{i.approvedVersion ?? '—'} → v{i.headVersion}
                </Text>
              </Table.Td>
              <Table.Td>{i.author ?? '—'}</Table.Td>
              <Table.Td maw={200}>
                <Changed item={i} />
              </Table.Td>
              <Table.Td>
                <Text size="xs" c="dimmed">
                  {formatRelative(i.updatedAt)}
                </Text>
              </Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
    </div>
  );
}

function Empty({ project, off }: { project: string; off: boolean }) {
  const canSettings = useCan('settings:write', project);
  if (off) {
    return (
      <EmptyState
        title="Review is off for this project"
        description="Versions of cases are not reviewed."
        action={
          canSettings ? (
            <Button component={Link} to={`/p/${project}/settings?tab=workflow`} size="xs" variant="default" data-testid="reviews-off-link">
              Open the workflow settings
            </Button>
          ) : undefined
        }
      />
    );
  }
  return <EmptyState title="Nothing waits for review" description="Changes to cases appear here when they need a reviewer." />;
}

function Queue({ project, query, off, mobile }: { project: string; query: ReviewQuery; off: boolean; mobile: boolean }) {
  const q = useReviewQueue(project, query, !off);
  const items = flatItems(q.data);
  if (off) return <Empty project={project} off />;
  return (
    <QueryState query={q}>
      {() =>
        items.length === 0 ? (
          <Empty project={project} off={false} />
        ) : (
          <Stack gap="sm">
            <QueueRows project={project} items={items} mobile={mobile} />
            {q.hasNextPage && (
              <Group justify="center">
                <Button variant="default" size="xs" loading={q.isFetchingNextPage} onClick={() => void q.fetchNextPage()} data-testid="reviews-more">
                  Load more
                </Button>
              </Group>
            )}
          </Stack>
        )
      }
    </QueryState>
  );
}

// Batch: review a set of changes as one pull request

type Scope = 'filter' | 'plan' | 'query';

interface Loaded {
  items: ReviewQueueItem[];
  note: string | null;
}

async function planNumbers(project: string, id: string): Promise<number[]> {
  const out: number[] = [];
  let before: string | undefined;
  for (let i = 0; i < 200; i++) {
    const page = await request<{ items: TestPlanItem[]; next?: string | null }>(withQuery(`${projectPath(project)}/test-plans/${encodeURIComponent(id)}/items`, { before, limit: 500 }));
    out.push(...page.items.map((x) => x.caseNumber));
    if (!page.next) break;
    before = page.next;
  }
  return out;
}

function BatchItem({ project, item, included, onToggle, open, onOpen }: { project: string; item: ReviewQueueItem; included: boolean; onToggle: (on: boolean) => void; open: boolean; onOpen: (on: boolean) => void }) {
  return (
    <Card withBorder radius="md" padding="sm" data-testid="batch-item" data-key={itemKey(item)}>
      <Stack gap="xs">
        <Group gap="xs" wrap="nowrap" align="flex-start">
          <Checkbox size="xs" mt={3} checked={included} onChange={(e) => onToggle(e.currentTarget.checked)} aria-label={`Include ${itemKey(item)} ${item.name}`} data-testid="batch-include" />
          <Stack gap={2} style={{ minWidth: 0, flex: 1 }}>
            <Group gap={6} wrap="wrap">
              <Text size="xs" c="dimmed" className="fp-mono">
                {itemKey(item)}
              </Text>
              <StateBadge state={item.reviewState} />
              <Text size="xs" c="dimmed" className="fp-mono">
                v{item.approvedVersion ?? '—'} → v{item.headVersion}
              </Text>
            </Group>
            <ItemName project={project} item={item} />
          </Stack>
          {item.kind === 'case' && (
            <Button size="compact-xs" variant="subtle" onClick={() => onOpen(!open)} aria-expanded={open} aria-label={`${open ? 'Hide' : 'Show'} changes of ${itemKey(item)}`} data-testid="batch-toggle">
              {open ? 'Hide changes' : 'Show changes'}
            </Button>
          )}
        </Group>
        {item.kind === 'case' && open && <ReviewDiff project={project} number={item.number} approved={item.approvedVersion ?? null} head={item.headVersion} />}
        {item.kind === 'sharedStep' && (
          <Text size="xs" c="dimmed">
            Open the shared step to read its changes.
          </Text>
        )}
      </Stack>
    </Card>
  );
}

function BatchResult({ project, result, items }: { project: string; result: ReviewBulkResult; items: ReviewQueueItem[] }) {
  const names = new Map(items.map((i) => [itemKey(i), i.name] as const));
  return (
    <Alert color={result.skipped.length > 0 ? 'yellow' : 'teal'} variant="light" title={`${String(result.applied)} decided, ${String(result.skipped.length)} skipped`} data-testid="batch-result" role="status">
      {result.skipped.length > 0 && (
        <Stack gap={4} component="ul" m={0} pl="md">
          {result.skipped.map((s) => (
            <li key={`${s.kind}-${String(s.number)}`} data-testid="batch-skipped" data-code={s.code}>
              <Anchor component={Link} to={itemLink(project, s)} size="sm">
                {itemKey(s)}
              </Anchor>{' '}
              <Text span size="sm">
                {names.get(itemKey(s)) ?? ''}: {SKIP_LABEL[s.code] ?? s.code}
              </Text>
            </li>
          ))}
        </Stack>
      )}
    </Alert>
  );
}

function Batch({ project, query, off }: { project: string; query: ReviewQuery; off: boolean }) {
  const canReview = useCan('cases:review', project);
  const plans = useManagedPlans(project, {});
  const bulk = useBulkReview(project);
  const [scope, setScope] = useState<Scope>('filter');
  const [plan, setPlan] = useState<string | null>(null);
  const [fpql, setFpql] = useState('');
  const [loading, setLoading] = useState(false);
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const [excluded, setExcluded] = useState<Set<string>>(new Set());
  const [opened, setOpened] = useState<Set<string>>(new Set());
  const [comment, setComment] = useState('');
  const [result, setResult] = useState<{ result: ReviewBulkResult; items: ReviewQueueItem[] } | null>(null);
  const planData = useMemo(() => (plans.data ?? []).map((p) => ({ value: p.id, label: p.name })), [plans.data]);
  const tooLong = bytesOf(comment) > 4000;

  const load = async () => {
    setLoading(true);
    setResult(null);
    try {
      const queue = await fetchWholeQueue(project, query);
      let items = queue;
      let note: string | null = null;
      if (scope !== 'filter') {
        const numbers = scope === 'plan' ? (plan ? await planNumbers(project, plan) : []) : await fetchQueryNumbers(project, fpql.trim());
        if (numbers === null) {
          note = 'Queries are not available on this server yet.';
          items = [];
        } else {
          const set = new Set(numbers);
          items = queue.filter((i) => i.kind === 'case' && set.has(i.number));
        }
      }
      items = items.slice(0, 500);
      setLoaded({ items, note });
      setExcluded(new Set());
      setOpened(new Set(items.slice(0, 10).map(itemKey)));
    } catch (e) {
      notifications.show({ color: 'red', title: 'Not loaded', message: errorMessage(e) });
    } finally {
      setLoading(false);
    }
  };

  const chosen = (loaded?.items ?? []).filter((i) => !excluded.has(itemKey(i)));
  const decide = (decision: ReviewDecision) => {
    const items = chosen;
    bulk.mutate(
      { items: items.map((i) => ({ kind: i.kind, number: i.number, version: i.headVersion })), decision, ...(comment.trim() ? { comment: comment.trim() } : {}) },
      {
        onSuccess: (r) => {
          setResult({ result: r, items });
          setComment('');
          setLoaded(null);
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Not saved', message: errorMessage(e) }),
      },
    );
  };

  if (off) return <Empty project={project} off />;
  const canLoad = scope === 'filter' || (scope === 'plan' ? !!plan : fpql.trim().length > 0);

  return (
    <Stack gap="md" data-testid="batch">
      <Text size="sm" c="dimmed">
        Read the changes of several cases one after another and decide on all of them at once. The queue filters above also apply here.
      </Text>
      <Group gap="sm" align="flex-end" wrap="wrap">
        <SegmentedControl
          aria-label="Batch scope"
          value={scope}
          onChange={(v) => {
            setScope(v);
            setLoaded(null);
          }}
          data={[
            { value: 'filter', label: 'Current filter' },
            { value: 'plan', label: 'Test plan' },
            { value: 'query', label: 'FPQL query' },
          ]}
          data-testid="batch-scope"
        />
        {scope === 'plan' && <Select label="Test plan" placeholder="Pick a plan" data={planData} value={plan} onChange={setPlan} searchable w={{ base: '100%', sm: 280 }} comboboxProps={{ withinPortal: true }} data-testid="batch-plan" />}
        {scope === 'query' && <TextInput label="FPQL query" placeholder='status = "draft" and tag = "smoke"' value={fpql} onChange={(e) => setFpql(e.currentTarget.value)} w={{ base: '100%', sm: 360 }} data-testid="batch-query" />}
        <Button size="xs" onClick={() => void load()} loading={loading} disabled={!canLoad} data-testid="batch-load">
          Load changes
        </Button>
      </Group>
      {result && <BatchResult project={project} result={result.result} items={result.items} />}
      {loaded?.note && (
        <Alert color="gray" variant="light" data-testid="batch-note">
          {loaded.note}
        </Alert>
      )}
      {loaded && !loaded.note && loaded.items.length === 0 && <Text size="sm" c="dimmed">Nothing in this scope waits for review.</Text>}
      {loaded && loaded.items.length > 0 && (
        <>
          <Text size="sm" fw={600} data-testid="batch-count">
            {chosen.length} of {loaded.items.length} selected
          </Text>
          <Stack gap="sm">
            {loaded.items.map((i) => {
              const k = itemKey(i);
              return (
                <BatchItem
                  key={k}
                  project={project}
                  item={i}
                  included={!excluded.has(k)}
                  onToggle={(on) =>
                    setExcluded((prev) => {
                      const next = new Set(prev);
                      if (on) next.delete(k);
                      else next.add(k);
                      return next;
                    })
                  }
                  open={opened.has(k)}
                  onOpen={(on) =>
                    setOpened((prev) => {
                      const next = new Set(prev);
                      if (on) next.add(k);
                      else next.delete(k);
                      return next;
                    })
                  }
                />
              );
            })}
          </Stack>
          {canReview ? (
            <Stack gap="xs" data-testid="batch-form">
              <Textarea label="Comment for all" description="Plain text, up to 4000 bytes." value={comment} onChange={(e) => setComment(e.currentTarget.value)} minRows={2} autosize error={tooLong ? 'The comment is longer than 4000 bytes' : undefined} data-testid="batch-comment" />
              <Group gap="xs">
                <Button size="xs" color="teal" leftSection={<IconCheck size={14} />} disabled={chosen.length === 0 || tooLong} loading={bulk.isPending} onClick={() => decide('approve')} data-testid="batch-approve">
                  Approve all
                </Button>
                <Button size="xs" color="red" variant="light" leftSection={<IconMessageX size={14} />} disabled={chosen.length === 0 || tooLong} loading={bulk.isPending} onClick={() => decide('request_changes')} data-testid="batch-request">
                  Request changes on all
                </Button>
              </Group>
            </Stack>
          ) : (
            <Text size="sm" c="dimmed">
              You need the cases:review permission to decide.
            </Text>
          )}
        </>
      )}
    </Stack>
  );
}

export default function Reviews() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const [sp, setSp] = useSearchParams();
  const mobile = useMediaQuery('(max-width: 48em)');
  const folders = useCaseFolders(key);
  const workflow = useCaseWorkflow(key);
  useReviewEvents(key);

  const view = sp.get('view') === 'batch' ? 'batch' : 'queue';
  const kind = sp.get('kind') === 'case' || sp.get('kind') === 'sharedStep' ? (sp.get('kind') as ReviewKind) : '';
  const state = (['pending', 'changes_requested', 'approved'] as const).find((s) => s === sp.get('state')) ?? 'pending';
  const who: Who = sp.get('who') === 'reviewer' ? 'reviewer' : sp.get('who') === 'author' ? 'author' : '';
  const folder = sp.get('folder');

  const set = (k: string, v: string | null) =>
    setSp(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (v) next.set(k, v);
        else next.delete(k);
        return next;
      },
      { replace: true },
    );

  const query: ReviewQuery = {
    ...(kind ? { kind } : {}),
    state: state,
    ...(who === 'reviewer' ? { reviewer: 'me' as const } : {}),
    ...(who === 'author' ? { author: 'me' as const } : {}),
    ...(folder ? { folder } : {}),
  };
  const folderData = useMemo(() => (folders.data ?? []).map((f) => ({ value: f.id, label: folderPathLabel(folders.data, f.id) })).sort((a, b) => a.label.localeCompare(b.label)), [folders.data]);
  const off = workflow.data?.review.mode === 'off';

  return (
    <>
      <PageHeader crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Reviews' }]} title="Reviews" subtitle="Versions of cases and shared steps that wait for a decision." />
      <Stack gap="md">
        <Group gap="sm" align="flex-end" wrap="wrap" data-testid="review-filters">
          <Select label="Kind" data={KIND_OPTIONS} value={kind} onChange={(v) => set('kind', v || null)} allowDeselect={false} w={{ base: '100%', xs: 150 }} comboboxProps={{ withinPortal: true }} data-testid="filter-kind" />
          <Select label="State" data={STATE_OPTIONS} value={state} onChange={(v) => set('state', v && v !== 'pending' ? v : null)} allowDeselect={false} w={{ base: '100%', xs: 190 }} comboboxProps={{ withinPortal: true }} data-testid="filter-state" />
          <Select label="Folder" placeholder="Any folder" data={folderData} value={folder} onChange={(v) => set('folder', v)} searchable clearable w={{ base: '100%', xs: 220 }} comboboxProps={{ withinPortal: true }} data-testid="filter-folder" />
          <SegmentedControl
            aria-label="Whose changes"
            value={who}
            onChange={(v) => set('who', v || null)}
            data={[
              { value: '', label: 'Everyone' },
              { value: 'reviewer', label: 'Waiting for me' },
              { value: 'author', label: 'Mine' },
            ]}
            data-testid="filter-who"
          />
        </Group>
        <Tabs value={view} onChange={(v) => set('view', v === 'batch' ? 'batch' : null)} keepMounted={false}>
          <Tabs.List mb="sm">
            <Tabs.Tab value="queue" data-testid="tab-queue">
              Queue
            </Tabs.Tab>
            <Tabs.Tab value="batch" leftSection={<IconChecks size={14} />} data-testid="tab-batch">
              Review as one PR
            </Tabs.Tab>
          </Tabs.List>
          <Tabs.Panel value="queue">
            <Queue project={key} query={query} off={off} mobile={mobile} />
          </Tabs.Panel>
          <Tabs.Panel value="batch">
            <Batch project={key} query={query} off={off} />
          </Tabs.Panel>
        </Tabs>
      </Stack>
    </>
  );
}
