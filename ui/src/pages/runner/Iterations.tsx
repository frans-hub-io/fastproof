import { Button, Card, Group, Menu, Modal, SimpleGrid, Stack, Table, Text, Textarea, Tooltip, UnstyledButton } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconBug, IconChevronLeft, IconChevronRight, IconListCheck } from '@tabler/icons-react';
import { useMediaQuery } from '@mantine/hooks';
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useMemo, useState } from 'react';
import { errorMessage, projectPath } from '../../api/client';
import { patchRunIteration, putIterationStep, useRunIteration, type RunIteration } from '../../api/parameters';
import { etagOf, getVersioned, isConflict, patchRunItem, tmk, type RunItem, type RunStatus, type StepStatus, type Versioned } from '../../api/tm';
import { isTypingTarget } from '../../lib/keys';
import { newBudget, substituteHtml, valuesLabel } from '../../lib/tm/parameters';
import { defectInfo } from '../../lib/tm/ui';
import { effectiveStatuses, flattenSteps, RUN_STATUS_LABEL, suggestedStatus, untestedLeaves } from '../../lib/tm/steps';
import { CaseHtml } from '../cases/CaseHtml';
import { RunStatusBadge } from '../cases/common';
import { DefectDrawer } from './DefectDrawer';
import { SharedHead, StatusButtons } from './RunSteps';
import { htmlFor } from './types';

const FINAL: RunStatus[] = ['passed', 'failed', 'blocked', 'skipped'];
const COLOR: Record<RunStatus, string> = {
  untested: 'var(--mantine-color-gray-5)',
  in_progress: 'var(--mantine-color-blue-5)',
  passed: 'var(--fp-status-passed, var(--mantine-color-green-6))',
  failed: 'var(--fp-status-failed, var(--mantine-color-red-6))',
  blocked: 'var(--mantine-color-orange-6)',
  skipped: 'var(--mantine-color-gray-6)',
};

interface LocalStep {
  status: StepStatus;
  actual: string;
}

const MOBILE_STATUS: { status: StepStatus; label: string; color: string }[] = [
  { status: 'passed', label: 'Pass', color: 'green' },
  { status: 'failed', label: 'Fail', color: 'red' },
  { status: 'blocked', label: 'Block', color: 'orange' },
  { status: 'skipped', label: 'Skip', color: 'gray' },
];

/** Step status buttons that fit a phone: four in a row, a second press on the current one resets it. */
function MobileStatus({ path, current, readOnly, onSet }: { path: string; current: StepStatus; readOnly: boolean; onSet: (s: StepStatus) => void }) {
  return (
    <SimpleGrid cols={4} spacing={4} w="100%" role="group" aria-label={`Status of step ${path}`}>
      {MOBILE_STATUS.map((b) => (
        <Button key={b.status} size="xs" h={40} px={4} color={b.color} variant={current === b.status ? 'filled' : 'light'} aria-pressed={current === b.status} disabled={readOnly} onClick={() => onSet(current === b.status ? 'untested' : b.status)} data-testid={`set-${b.status}`}>
          {b.label}
        </Button>
      ))}
    </SimpleGrid>
  );
}

function firstOpen(its: readonly RunIteration[]): number {
  const i = its.findIndex((x) => !FINAL.includes(x.status));
  return i >= 0 ? i : 0;
}

/** The strip of iterations: one chip per row, `[` and `]` move between them. */
function Strip({ its, current, columns, onPick }: { its: readonly RunIteration[]; current: number; columns: readonly string[]; onPick: (i: number) => void }) {
  return (
    <Group gap={6} wrap="wrap" role="tablist" aria-label="Iterations" data-testid="iteration-strip">
      {its.map((it, i) => (
        <Tooltip key={it.rowId} label={`${valuesLabel(columns, it.values)} · ${RUN_STATUS_LABEL[it.status]}`}>
          <UnstyledButton
            role="tab"
            aria-selected={i === current}
            aria-label={`Iteration ${String(it.position)}: ${valuesLabel(columns, it.values)}, ${RUN_STATUS_LABEL[it.status]}`}
            onClick={() => onPick(i)}
            className="fp-tm-iteration-chip"
            data-testid="iteration-chip"
            data-status={it.status}
            data-current={i === current}
            style={{
              display: 'inline-flex',
              alignItems: 'center',
              gap: 6,
              minHeight: 32,
              padding: '2px 10px',
              borderRadius: 16,
              border: `1px solid ${i === current ? 'var(--mantine-color-blue-6)' : 'var(--mantine-color-default-border)'}`,
              background: i === current ? 'var(--mantine-color-blue-light)' : 'transparent',
            }}
          >
            <span aria-hidden style={{ width: 10, height: 10, borderRadius: 5, background: COLOR[it.status], flex: 'none' }} />
            <Text span size="sm" fw={i === current ? 600 : 400} className="fp-mono">
              #{it.position}
            </Text>
          </UnstyledButton>
        </Tooltip>
      ))}
    </Group>
  );
}

/**
 * The runner of a parameterized case: one iteration per enabled parameter row, each with its own step results and
 * status. The item's status is derived from the iterations by the server; "Apply to all iterations" is explicit.
 */
export function Iterations({ projectKey, launchId, item, readOnly = false, onAnnounce }: { projectKey: string; launchId: string; item?: RunItem; readOnly?: boolean; onAnnounce?: (m: string) => void }) {
  const qc = useQueryClient();
  const its = useMemo(() => item?.iterations ?? [], [item?.iterations]);
  const [pos, setPos] = useState<{ item: string; idx: number } | null>(null);
  const [local, setLocal] = useState<Map<string, Map<string, LocalStep>>>(() => new Map());
  const [comment, setComment] = useState<{ row: string; text: string } | null>(null);
  const [applyAll, setApplyAll] = useState<RunStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [defect, setDefect] = useState<null | 'row' | 'item'>(null);
  const mobile = useMediaQuery('(max-width: 48em)');
  const idx = item && pos?.item === item.id ? Math.min(pos.idx, Math.max(0, its.length - 1)) : firstOpen(its);
  const row = its[idx];
  const omitted = !!row?.stepResultsOmitted;
  const whole = useRunIteration(projectKey, item?.id ?? '', row?.rowId ?? '', item?.executionNo ?? 0, item?.revision ?? 0, !!item && omitted);
  const it = row && omitted && whole.data ? { ...row, stepResults: whole.data.stepResults, stepResultsOmitted: false } : row;
  const loadingSteps = omitted && !whole.data;
  const say = (m: string) => onAnnounce?.(m);

  useEffect(() => {
    setLocal(new Map());
    // A new execution (Run again) starts at its first open iteration.
    setPos((p) => (p && p.item === item?.id ? null : p));
  }, [item?.id, item?.executionNo]);

  useEffect(() => {
    if (!item) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || isTypingTarget(e.target) || e.ctrlKey || e.metaKey || e.altKey) return;
      if (document.querySelector('[role="dialog"]')) return;
      if (e.key === 'd' && !readOnly && row && FINAL.includes(row.status)) {
        e.preventDefault();
        setDefect('row');
        return;
      }
      if (e.key !== '[' && e.key !== ']') return;
      e.preventDefault();
      const next = Math.max(0, Math.min(its.length - 1, idx + (e.key === ']' ? 1 : -1)));
      setPos({ item: item.id, idx: next });
      const n = its[next];
      if (n) say(`Iteration ${String(n.position)}: ${valuesLabel(item.parameterColumns, n.values)}`);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  if (!item || item.iterationCount === 0 || !it) return null;

  const saved = new Map(it.stepResults.map((r) => [r.path, { status: r.status, actual: r.actual }] as const));
  const mine = local.get(it.rowId);
  const steps = new Map<string, LocalStep>([...saved, ...(mine ?? new Map<string, LocalStep>())]);
  const statusMap = new Map([...steps].map(([p, s]) => [p, s.status] as const));
  const effective = effectiveStatuses(item.steps, statusMap);
  const base = suggestedStatus(item.steps, statusMap);
  const suggested = base === 'in_progress' ? suggestedStatus(item.steps, statusMap, true) : base;
  const untested = untestedLeaves(item.steps, statusMap);
  const flat = flattenSteps(item.steps);
  const values = it.values;
  const budget = newBudget();
  const html = (s: string | undefined) => substituteHtml(s ?? '', values, budget);
  const itemDefect = defectInfo(item.defect);
  const rowDefect = defectInfo(it.defect);
  const notPassed = its.filter((x) => x.status === 'failed' || x.status === 'blocked').length;
  const refresh = () => void qc.invalidateQueries({ queryKey: tmk.runItem(projectKey, item.id) });
  const afterItem = (v: Versioned<RunItem>) => {
    qc.setQueryData(tmk.runItem(projectKey, item.id), v);
    void qc.invalidateQueries({ queryKey: tmk.runItems(projectKey, launchId) });
    void qc.invalidateQueries({ queryKey: tmk.launch(projectKey, launchId) });
  };
  const fresh = () => getVersioned<RunItem>(`${projectPath(projectKey)}/run-items/${encodeURIComponent(item.id)}`);

  const setStep = (path: string, status: StepStatus, actual?: string) => {
    if (readOnly) return;
    const row = it.rowId;
    setLocal((m) => {
      const n = new Map(m);
      const r = new Map(n.get(row) ?? []);
      r.set(path, { status, actual: actual ?? steps.get(path)?.actual ?? '' });
      n.set(row, r);
      return n;
    });
    say(`Iteration ${String(it.position)}, step ${path}: ${RUN_STATUS_LABEL[status].toLowerCase()}`);
    putIterationStep(projectKey, item.id, row, path, { status, ...(actual !== undefined ? { actual } : {}) }).then(refresh, (e: unknown) => {
      notifications.show({ color: 'red', title: `Step ${path} not saved`, message: errorMessage(e) });
      refresh();
    });
  };

  const finish = async (status: RunStatus) => {
    if (readOnly) return;
    setBusy(true);
    const row = it.rowId;
    const patch = { status, ...(untested.length && FINAL.includes(status) ? { untestedStepsTo: 'skipped' as const } : {}) };
    try {
      let v;
      try {
        v = await patchRunIteration(projectKey, item.id, row, etagOf(item.revision), patch);
      } catch (e) {
        if (!isConflict(e)) throw e;
        v = await patchRunIteration(projectKey, item.id, row, (await fresh()).etag, patch);
      }
      afterItem(v);
      setLocal((m) => {
        const n = new Map(m);
        n.delete(row);
        return n;
      });
      const next = v.value.iterations.findIndex((x, i) => i > idx && !FINAL.includes(x.status));
      say(`Iteration ${String(it.position)}: ${RUN_STATUS_LABEL[status].toLowerCase()}. The case is ${RUN_STATUS_LABEL[v.value.status].toLowerCase()}.`);
      if (next >= 0) setPos({ item: item.id, idx: next });
    } catch (e) {
      notifications.show({ color: 'red', title: `Iteration ${String(it.position)} not finished`, message: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  const saveComment = async () => {
    if (!comment || readOnly) return;
    const c = comment;
    setComment(null);
    try {
      afterItem(await patchRunIteration(projectKey, item.id, c.row, (await fresh()).etag, { comment: c.text }));
    } catch (e) {
      notifications.show({ color: 'red', title: 'Comment not saved', message: errorMessage(e) });
    }
  };

  const doApplyAll = async (status: RunStatus) => {
    setApplyAll(null);
    setBusy(true);
    try {
      afterItem(await patchRunItem(projectKey, item.id, (await fresh()).etag, { status, allIterations: true, ...(FINAL.includes(status) ? { untestedStepsTo: 'skipped' as const } : {}) }));
      setLocal(new Map());
      say(`Every iteration is ${RUN_STATUS_LABEL[status].toLowerCase()}`);
    } catch (e) {
      notifications.show({ color: 'red', title: 'Not applied', message: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  const done = its.filter((x) => FINAL.includes(x.status)).length;

  return (
    <Stack gap="sm" data-testid="iterations">
      <Card withBorder radius="lg" padding="md">
        <Stack gap="xs">
          <Group justify="space-between" wrap="wrap" gap="xs">
            <Group gap="xs">
              <Text size="sm" fw={600}>
                Iterations
              </Text>
              <Text size="xs" c="dimmed" data-testid="iterations-progress">
                {done} / {its.length} done
              </Text>
            </Group>
            <Group gap={4}>
              <Tooltip label="Previous iteration ([)">
                <Button size="compact-xs" variant="default" aria-label="Previous iteration" disabled={idx === 0} onClick={() => setPos({ item: item.id, idx: idx - 1 })} leftSection={<IconChevronLeft size={12} />}>
                  [
                </Button>
              </Tooltip>
              <Tooltip label="Next iteration (])">
                <Button size="compact-xs" variant="default" aria-label="Next iteration" disabled={idx >= its.length - 1} onClick={() => setPos({ item: item.id, idx: idx + 1 })} rightSection={<IconChevronRight size={12} />}>
                  ]
                </Button>
              </Tooltip>
              <Tooltip position="bottom" multiline w={220} label="A defect on every failed or blocked iteration">
                <Button size="compact-xs" variant="subtle" leftSection={<IconBug size={12} />} disabled={readOnly || notPassed === 0} onClick={() => setDefect('item')} data-testid="item-defect">
                  Defect…
                </Button>
              </Tooltip>
              <Menu withinPortal position="bottom-end">
                <Menu.Target>
                  <Button size="compact-xs" variant="subtle" leftSection={<IconListCheck size={12} />} disabled={readOnly || busy} data-testid="iterations-apply-all">
                    Apply to all…
                  </Button>
                </Menu.Target>
                <Menu.Dropdown>
                  <Menu.Label>Set every iteration to</Menu.Label>
                  {FINAL.map((s) => (
                    <Menu.Item key={s} onClick={() => setApplyAll(s)} data-testid={`apply-all-${s}`}>
                      {RUN_STATUS_LABEL[s]}
                    </Menu.Item>
                  ))}
                </Menu.Dropdown>
              </Menu>
            </Group>
          </Group>
          {itemDefect && (
            <Text size="xs" data-testid="item-defect-info">
              Defect: {itemDefect.label}
              {itemDefect.issue ? ` · ${itemDefect.issue}` : ''}
            </Text>
          )}
          <Strip its={its} current={idx} columns={item.parameterColumns} onPick={(i) => setPos({ item: item.id, idx: i })} />
        </Stack>
      </Card>
      <Card withBorder radius="lg" padding="md" data-testid="iteration" data-row={it.rowId} data-status={it.status}>
        <Stack gap="sm">
          <Group justify="space-between" wrap="wrap" gap="xs">
            <Group gap="xs">
              <Text fw={600}>Iteration {it.position}</Text>
              <RunStatusBadge status={it.status} />
            </Group>
            <Group gap="xs">
              {it.executedBy && (
                <Text size="xs" c="dimmed">
                  by {it.executedBy}
                </Text>
              )}
              <Tooltip position="bottom-end" multiline w={220} label={FINAL.includes(it.status) ? 'Defect for this iteration (d)' : 'Finish the iteration first: a defect belongs to its result'}>
                <Button size="compact-xs" variant="default" leftSection={<IconBug size={12} />} disabled={readOnly || !FINAL.includes(it.status)} onClick={() => setDefect('row')} data-testid="iteration-defect">
                  Defect
                </Button>
              </Tooltip>
            </Group>
          </Group>
          {rowDefect && (
            <Text size="xs" data-testid="iteration-defect-info">
              Defect: {rowDefect.label}
              {rowDefect.issue ? ` · ${rowDefect.issue}` : ''}
              {rowDefect.comment ? ` · ${rowDefect.comment}` : ''}
            </Text>
          )}
          <Table.ScrollContainer minWidth={240}>
            <Table withTableBorder verticalSpacing={2} data-testid="iteration-values">
              <Table.Tbody>
                {item.parameterColumns.map((c) => (
                  <Table.Tr key={c}>
                    <Table.Th w="35%" className="fp-mono">
                      {c}
                    </Table.Th>
                    <Table.Td>
                      <Text size="sm" style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>
                        {values[c] ?? ''}
                      </Text>
                    </Table.Td>
                  </Table.Tr>
                ))}
              </Table.Tbody>
            </Table>
          </Table.ScrollContainer>
          {item.preconditionsHtml && (
            <Stack gap={2}>
              <Text size="xs" fw={600} c="dimmed">
                Preconditions
              </Text>
              <CaseHtml html={html(item.preconditionsHtml)} project={projectKey} />
            </Stack>
          )}
          {loadingSteps && (
            <Text size="sm" c="dimmed" role="status" data-testid="iteration-loading">
              Loading the step results of this iteration…
            </Text>
          )}
          <Stack gap={6} hidden={loadingSteps}>
            {flat.map((f) => {
              const h = htmlFor(item, f.path);
              const own = steps.get(f.path);
              const st = own?.status ?? 'untested';
              return (
                <Card key={f.path} withBorder radius="md" padding="xs" style={{ marginLeft: f.depth * 16 }} data-testid="iteration-step" data-path={f.path} data-status={effective.get(f.path) ?? 'untested'}>
                  <Stack gap={4}>
                    <Group justify="space-between" gap="xs" wrap="wrap" align="flex-start">
                      <Group gap={6} wrap="nowrap" align="flex-start" style={{ minWidth: 0, flex: '1 1 240px' }}>
                        <Text size="xs" c="dimmed" className="fp-mono" mt={2}>
                          {f.path}
                        </Text>
                        <div style={{ minWidth: 0 }}>{f.step.shared ? <SharedHead item={item} step={f.step} /> : <CaseHtml html={html(h?.action)} project={projectKey} />}</div>
                      </Group>
                      {!f.hasChildren && (
                        mobile ? <MobileStatus path={f.path} current={st} readOnly={readOnly} onSet={(s) => setStep(f.path, s)} /> : <StatusButtons path={f.path} current={st} readOnly={readOnly} onSet={(s) => setStep(f.path, s)} />
                      )}
                    </Group>
                    {f.step.expected && (
                      <div className="fp-tm-expected">
                        <Text span size="xs" c="dimmed">
                          Expected:{' '}
                        </Text>
                        <CaseHtml html={html(h?.expected)} project={projectKey} />
                      </div>
                    )}
                    {f.step.data && (
                      <div className="fp-tm-expected">
                        <Text span size="xs" c="dimmed">
                          Data:{' '}
                        </Text>
                        <CaseHtml html={html(h?.data)} project={projectKey} />
                      </div>
                    )}
                    {(st === 'failed' || st === 'blocked' || own?.actual) && !f.hasChildren && (
                      <Textarea
                        size="xs"
                        label="Actual result"
                        defaultValue={own?.actual ?? ''}
                        key={`${it.rowId}:${f.path}:${String(item.revision)}`}
                        autosize
                        minRows={1}
                        readOnly={readOnly}
                        onBlur={(e) => {
                          const a = e.currentTarget.value;
                          if (a !== (own?.actual ?? '')) setStep(f.path, st, a);
                        }}
                        data-testid="iteration-actual"
                      />
                    )}
                  </Stack>
                </Card>
              );
            })}
          </Stack>
          <Textarea
            size="xs"
            label="Comment on this iteration"
            value={comment?.row === it.rowId ? comment.text : it.comment}
            onChange={(e) => setComment({ row: it.rowId, text: e.currentTarget.value })}
            onBlur={() => void saveComment()}
            autosize
            minRows={1}
            readOnly={readOnly}
            data-testid="iteration-comment"
          />
          <Group justify="space-between" wrap="wrap" gap="xs">
            <Group gap={6}>
              <Text size="sm">{suggested === 'untested' ? 'Record the steps, then finish the iteration.' : 'Suggested:'}</Text>
              {suggested !== 'untested' && <RunStatusBadge status={suggested} />}
              {base === 'in_progress' && untested.length > 0 && (
                <Text size="xs" c="dimmed">
                  {untested.length} untested step{untested.length === 1 ? '' : 's'} become skipped
                </Text>
              )}
            </Group>
          </Group>
          <Group gap="xs" wrap="wrap">
            {FINAL.map((s) => (
              <Button
                key={s}
                size="xs"
                variant={s === suggested ? 'filled' : 'light'}
                color={s === 'passed' ? 'green' : s === 'failed' ? 'red' : s === 'blocked' ? 'orange' : 'gray'}
                onClick={() => void finish(s)}
                disabled={readOnly || busy || loadingSteps}
                data-testid={`iteration-finish-${s}`}
              >
                {RUN_STATUS_LABEL[s]}
              </Button>
            ))}
          </Group>
        </Stack>
      </Card>
      {defect && (
        <DefectDrawer
          key={defect === 'row' ? it.rowId : 'item'}
          project={projectKey}
          item={item}
          local={new Map()}
          iteration={defect === 'row' ? it : undefined}
          opened
          onClose={() => setDefect(null)}
        />
      )}
      <Modal opened={!!applyAll} onClose={() => setApplyAll(null)} title="Apply to every iteration" attributes={{ content: { 'data-testid': 'apply-all-confirm' } }}>
        {applyAll && (
          <Stack gap="sm">
            <Text size="sm">
              Set all {its.length} iterations to <b>{RUN_STATUS_LABEL[applyAll]}</b>, including the ones already finished? Untested steps become skipped.
            </Text>
            <Group justify="flex-end" gap="xs">
              <Button variant="default" onClick={() => setApplyAll(null)}>
                Cancel
              </Button>
              <Button onClick={() => void doApplyAll(applyAll)} data-testid="apply-all-ok">
                Apply to all
              </Button>
            </Group>
          </Stack>
        )}
      </Modal>
    </Stack>
  );
}
