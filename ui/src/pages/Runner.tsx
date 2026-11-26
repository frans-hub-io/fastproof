import { Alert, Badge, Button, Card, Checkbox, Drawer, Group, Kbd, Loader, Modal, Stack, Text, Tooltip } from '@mantine/core';
import { EmptyState, useCan, useMe } from '@franshub/ui-shell';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconArrowLeft, IconArrowRight, IconBug, IconCloudCheck, IconCloudOff, IconKeyboard, IconListDetails, IconPlayerTrackNext, IconRefresh, IconRotateClockwise } from '@tabler/icons-react';
import { useQueryClient } from '@tanstack/react-query';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router';
import { ApiError, errorMessage, projectPath } from '../api/client';
import { useLaunch, useProject } from '../api/hooks';
import { etagOf, getVersioned, isConflict, patchRunItem, putRunStep, rerunRunItem, tmk, uploadRunAttachment, useRunItem, useRunItems, useTmEvents, type RunItem, type RunStatus, type StepStatus, type TmLaunch } from '../api/tm';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { isTypingTarget } from '../lib/keys';
import { caseLabel } from '../lib/tm/filters';
import { setSingleKeysEnabled, singleKeysEnabled } from '../lib/tm/prefs';
import { effectiveStatuses, flattenSteps, RUN_STATUS_LABEL, suggestedStatus, untestedLeaves } from '../lib/tm/steps';
import { CaseHtml } from './cases/CaseHtml';
import { LiveRegion, RunStatusBadge } from './cases/common';
import { DefectDrawer } from './runner/DefectDrawer';
import { Executions } from './runner/Executions';
import { HelpModal } from './runner/HelpModal';
import { runnerAction } from './runner/keymap';
import { RunQueue, type QueueFilter } from './runner/RunQueue';
import { Iterations } from './runner/Iterations';
import { SharedRefresh } from './runner/SharedRefresh';
import { RunSteps, SharedHead, StatusButtons, StepFields } from './runner/RunSteps';
import { SaveQueue, type QueueSnapshot } from './runner/saveQueue';
import { emptyLocal, htmlFor, isOpen, type LocalStep } from './runner/types';

const FINAL: RunStatus[] = ['passed', 'failed', 'blocked', 'skipped'];
const TEXT_DELAY = 600;

function Elapsed({ since }: { since: number }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);
  const s = Math.max(0, Math.floor((now - since) / 1000));
  const mm = `${String(Math.floor((s % 3600) / 60)).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}`;
  const text = s >= 3600 ? `${String(Math.floor(s / 3600))}:${mm}` : mm;
  return (
    <Tooltip label="Time on this case">
      <Text size="xs" c="dimmed" className="fp-mono" aria-label={`Time on this case ${text}`} role="timer" aria-live="off">
        {text}
      </Text>
    </Tooltip>
  );
}

function SaveIndicator({ s, onRetry }: { s: QueueSnapshot; onRetry: () => void }) {
  if (s.state === 'retrying') {
    return (
      <Group gap={4} wrap="nowrap" data-testid="save-state" data-state="retrying">
        <IconCloudOff size={14} color="var(--mantine-color-orange-6)" aria-hidden />
        <Text size="xs" c="orange">
          Not saved ({s.pending}), retrying
        </Text>
        <Button size="compact-xs" variant="subtle" onClick={onRetry}>
          Retry now
        </Button>
      </Group>
    );
  }
  return (
    <Group gap={4} wrap="nowrap" data-testid="save-state" data-state={s.state}>
      {s.state === 'saving' ? <Loader size={12} /> : <IconCloudCheck size={14} color="var(--fp-status-passed)" aria-hidden />}
      <Text size="xs" c="dimmed">
        {s.state === 'saving' ? 'Saving…' : s.state === 'saved' ? 'Saved' : 'Up to date'}
      </Text>
    </Group>
  );
}

function isFatal(e: unknown): boolean {
  return e instanceof ApiError && e.status >= 400 && e.status < 500 && e.status !== 408 && e.status !== 429 && e.status !== 412;
}

export default function RunnerPage() {
  const { key = '', id = '', itemId } = useParams();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const { me } = useMe();
  const canRun = useCan('runs:execute', key);
  const launch = useLaunch(key, id);
  const project = useProject(key);
  const l: TmLaunch | undefined = launch.data;
  const all = useRunItems(key, id, {});
  const mobile = useMediaQuery('(max-width: 48em)');
  const meName = me?.name ?? '';
  const [mineSetting, setMine] = useState<boolean | null>(null);
  const [filter, setFilter] = useState<QueueFilter>('all');
  const [overrides, setOverrides] = useState<Map<string, RunStatus>>(() => new Map());
  const [singleKeys, setSingleKeys] = useState(singleKeysEnabled);
  const [help, setHelp] = useState(false);
  const [defect, setDefect] = useState(false);
  const [queueDrawer, setQueueDrawer] = useState(false);
  const [finishAsk, setFinishAsk] = useState<null | { untested: number; suggested: RunStatus }>(null);
  const [announce, setAnnounce] = useState('');
  const [stepPos, setStepPos] = useState<{ id: string; idx: number } | null>(null);
  const [uploading, setUploading] = useState(false);
  const [skipUntested, setSkipUntested] = useState(true);
  const [saveState, setSaveState] = useState<QueueSnapshot>({ state: 'idle', pending: 0, lastError: null });
  const [saver] = useState(() => new SaveQueue());
  const [localState, setLocalState] = useState<ReadonlyMap<string, ReadonlyMap<string, LocalStep>>>(() => new Map());
  const [since, setSince] = useState(0);
  const localRef = useRef(new Map<string, Map<string, LocalStep>>());
  const revRef = useRef(new Map<string, number>());
  const pendingRef = useRef(new Map<string, number>());
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const openedAt = useRef(new Map<string, number>());
  const publish = () => setLocalState(new Map(localRef.current));
  const actualRef = useRef<HTMLTextAreaElement>(null);
  const commentRef = useRef<HTMLTextAreaElement>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const titleRef = useRef<HTMLHeadingElement>(null);
  const focusAfter = useRef<null | 'actual' | 'step'>(null);

  useEffect(() => saver.subscribe(setSaveState), [saver]);
  useEffect(() => () => saver.dispose(), [saver]);
  useEffect(() => {
    const onPrefs = () => setSingleKeys(singleKeysEnabled());
    window.addEventListener('fastproof-prefs', onPrefs);
    return () => window.removeEventListener('fastproof-prefs', onPrefs);
  }, []);
  useEffect(() => {
    if (saveState.pending === 0) return;
    const warn = (e: BeforeUnloadEvent) => e.preventDefault();
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [saveState.pending]);

  const items = useMemo(() => all.data ?? [], [all.data]);
  const statusOf = useCallback((i: { id: string; status: RunStatus }) => overrides.get(i.id) ?? i.status, [overrides]);
  const mine = mineSetting ?? items.some((i) => i.assignee === meName);
  const queue = useMemo(() => items.filter((i) => (!mine || i.assignee === meName) && (filter === 'all' || (filter === 'open') === isOpen(statusOf(i)))), [items, mine, meName, filter, statusOf]);
  const runPath = `/p/${key}/launches/${id}/run`;

  useEffect(() => {
    if (itemId || !queue.length) return;
    const first = queue.find((i) => isOpen(statusOf(i))) ?? queue[0];
    if (first) void navigate(`${runPath}/${first.id}`, { replace: true });
  }, [itemId, queue, statusOf, navigate, runPath]);

  const itemQ = useRunItem(key, itemId);
  const item = itemQ.data?.value;
  const closed = l?.manual?.state === 'closed';
  const readOnly = !canRun || closed;

  useEffect(() => {
    const v = itemQ.data;
    if (!v) return;
    const it = v.value;
    const known = revRef.current.get(it.id) ?? -1;
    if (!localRef.current.has(it.id) || ((pendingRef.current.get(it.id) ?? 0) === 0 && it.revision > known)) {
      localRef.current.set(it.id, new Map(it.stepResults.map((r) => [r.path, { status: r.status, actual: r.actual, comment: r.comment, attachments: r.attachments }] as const)));
      revRef.current.set(it.id, it.revision);
      publish();
    }
  }, [itemQ.data]);

  const flat = useMemo(() => (item ? flattenSteps(item.steps) : []), [item]);
  const local = (item ? localState.get(item.id) : undefined) ?? new Map<string, LocalStep>();
  const statusMap = new Map([...local.entries()].map(([p, s]) => [p, s.status] as const));
  const effective = item ? effectiveStatuses(item.steps, statusMap) : new Map<string, StepStatus>();
  const suggested = item ? suggestedStatus(item.steps, statusMap) : 'untested';
  const untested = item ? untestedLeaves(item.steps, statusMap) : [];
  const itemStatus = item ? statusOf(item) : 'untested';
  const defaultIdx = useMemo(() => {
    if (!item) return 0;
    const results = new Map(item.stepResults.map((r) => [r.path, r.status] as const));
    const i = flattenSteps(item.steps).findIndex((f) => !f.hasChildren && (results.get(f.path) ?? 'untested') === 'untested');
    return i >= 0 ? i : 0;
  }, [item?.id]); // eslint-disable-line react-hooks/exhaustive-deps
  const stepIdx = stepPos && item && stepPos.id === item.id ? stepPos.idx : defaultIdx;
  const itemId2 = item?.id;
  const setStepIdx = useCallback(
    (v: number | ((i: number) => number)) => {
      if (!itemId2) return;
      setStepPos((prev) => {
        const base = prev && prev.id === itemId2 ? prev.idx : defaultIdx;
        return { id: itemId2, idx: typeof v === 'function' ? v(base) : v };
      });
    },
    [itemId2, defaultIdx],
  );

  useEffect(() => {
    if (!item) return;
    if (!openedAt.current.has(item.id)) openedAt.current.set(item.id, Date.now());
    setSince(openedAt.current.get(item.id) ?? 0);
    requestAnimationFrame(() => titleRef.current?.focus());
  }, [item?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const f = focusAfter.current;
    if (!f) return;
    focusAfter.current = null;
    if (f === 'actual') actualRef.current?.focus();
    else document.querySelector<HTMLElement>('[data-testid="run-step"][data-current="true"], [data-testid="mobile-step"]')?.focus();
  });

  useTmEvents(key, {
    runItemUpdated: (e) => {
      if (e.launchId !== id) return;
      if ((revRef.current.get(e.id) ?? -1) >= e.revision) return;
      void qc.invalidateQueries({ queryKey: tmk.runItems(key, id) });
      if (e.id === itemId && (pendingRef.current.get(e.id) ?? 0) === 0) void qc.invalidateQueries({ queryKey: tmk.runItem(key, e.id) });
    },
    manualRunUpdated: (e) => {
      if (e.launchId === id) void qc.invalidateQueries({ queryKey: tmk.launch(key, id) });
    },
  });

  const track = (itemKey: string, delta: number) => pendingRef.current.set(itemKey, Math.max(0, (pendingRef.current.get(itemKey) ?? 0) + delta));

  const enqueueStep = useCallback(
    (it: RunItem, path: string) => {
      track(it.id, 1);
      saver.enqueue(`${it.id}:${path}`, async () => {
        const latest = localRef.current.get(it.id)?.get(path) ?? emptyLocal();
        try {
          const ack = await putRunStep(key, it.id, path, { status: latest.status, actual: latest.actual, comment: latest.comment, attachmentIds: latest.attachments.map((a) => a.id) });
          revRef.current.set(it.id, ack.revision);
          setOverrides((o) => {
            const prev = o.get(it.id);
            // A finish recorded locally (queued after this step) wins over the step's interim item status.
            if (prev && FINAL.includes(prev) && !FINAL.includes(ack.itemStatus)) return o;
            return new Map(o).set(it.id, ack.itemStatus);
          });
        } catch (e) {
          if (!isFatal(e)) throw e;
          notifications.show({ color: 'red', title: `Step ${path} not saved`, message: errorMessage(e) });
        }
        track(it.id, -1);
      });
    },
    [key, saver],
  );

  const setLocal = (it: RunItem, path: string, patch: Partial<LocalStep>) => {
    const m = new Map(localRef.current.get(it.id) ?? []);
    m.set(path, { ...(m.get(path) ?? emptyLocal()), ...patch });
    localRef.current.set(it.id, m);
    publish();
  };

  const flushText = (it: RunItem) => {
    for (const [k, t] of timers.current) {
      if (!k.startsWith(`${it.id}:`)) continue;
      clearTimeout(t);
      timers.current.delete(k);
      enqueueStep(it, k.slice(it.id.length + 1));
    }
  };

  const setStep = (path: string, status: StepStatus, advance = true) => {
    if (!item || readOnly) return;
    const t = timers.current.get(`${item.id}:${path}`);
    if (t) {
      clearTimeout(t);
      timers.current.delete(`${item.id}:${path}`);
    }
    setLocal(item, path, { status });
    enqueueStep(item, path);
    setAnnounce(`Step ${path}: ${RUN_STATUS_LABEL[status].toLowerCase()}`);
    if (status === 'failed') focusAfter.current = 'actual';
    else if (advance && status !== 'untested') {
      const i = flat.findIndex((f) => f.path === path);
      if (i >= 0 && i < flat.length - 1) {
        setStepIdx(i + 1);
        focusAfter.current = 'step';
      }
    }
  };

  const setText = (path: string, field: 'actual' | 'comment', value: string) => {
    if (!item || readOnly) return;
    setLocal(item, path, { [field]: value });
    const k = `${item.id}:${path}`;
    const prev = timers.current.get(k);
    if (prev) clearTimeout(prev);
    const it = item;
    timers.current.set(
      k,
      setTimeout(() => {
        timers.current.delete(k);
        enqueueStep(it, path);
      }, TEXT_DELAY),
    );
  };

  const addFiles = async (path: string, files: FileList | null) => {
    if (!item || !files?.length || readOnly) return;
    setUploading(true);
    try {
      const refs = [];
      for (const f of Array.from(files).slice(0, 20)) refs.push(await uploadRunAttachment(key, item.id, f, f.name));
      const cur = localRef.current.get(item.id)?.get(path) ?? emptyLocal();
      setLocal(item, path, { attachments: [...cur.attachments, ...refs].slice(0, 20) });
      enqueueStep(item, path);
      setAnnounce(`${String(refs.length)} file${refs.length === 1 ? '' : 's'} attached to step ${path}`);
    } catch (e) {
      notifications.show({ color: 'red', title: 'Not attached', message: errorMessage(e) });
    } finally {
      setUploading(false);
      if (fileRef.current) fileRef.current.value = '';
    }
  };

  const goto = (dir: 1 | -1) => {
    if (!item) return;
    const i = queue.findIndex((x) => x.id === item.id);
    const next = queue[i + dir];
    if (next) void navigate(`${runPath}/${next.id}`, { replace: true });
  };

  const nextOpenAfter = (currentId: string, done: Map<string, RunStatus>) => {
    const i = queue.findIndex((x) => x.id === currentId);
    const order = [...queue.slice(i + 1), ...queue.slice(0, Math.max(0, i))];
    return order.find((x) => isOpen(done.get(x.id) ?? x.status));
  };

  const finish = (status: RunStatus, skipRest: boolean, advance = true) => {
    if (!item || readOnly) return;
    const it = item;
    flushText(it);
    if (skipRest) for (const p of untestedLeaves(it.steps, statusMap)) setLocal(it, p, { status: 'skipped' });
    const done = new Map(overrides).set(it.id, status);
    setOverrides(done);
    setFinishAsk(null);
    track(it.id, 1);
    saver.enqueue(`${it.id}:finish`, async () => {
      const patch = { status, ...(skipRest ? { untestedStepsTo: 'skipped' as const } : {}) };
      try {
        let v;
        try {
          v = await patchRunItem(key, it.id, etagOf(revRef.current.get(it.id) ?? it.revision), patch);
        } catch (e) {
          if (!isConflict(e)) throw e;
          const fresh = await getVersioned<RunItem>(`${projectPath(key)}/run-items/${encodeURIComponent(it.id)}`);
          v = await patchRunItem(key, it.id, fresh.etag, patch);
        }
        revRef.current.set(it.id, v.value.revision);
        qc.setQueryData(tmk.runItem(key, it.id), v);
        void qc.invalidateQueries({ queryKey: tmk.launch(key, id) });
      } catch (e) {
        if (!isFatal(e)) throw e;
        notifications.show({ color: 'red', title: `${caseLabel(it.caseNumber)} not finished`, message: errorMessage(e) });
        setOverrides((o) => {
          const n = new Map(o);
          n.delete(it.id);
          return n;
        });
      }
      track(it.id, -1);
    });
    if (!advance) {
      setAnnounce(`${caseLabel(it.caseNumber)} finished: ${RUN_STATUS_LABEL[status].toLowerCase()}`);
      return;
    }
    const next = nextOpenAfter(it.id, done);
    setAnnounce(`${caseLabel(it.caseNumber)} finished: ${RUN_STATUS_LABEL[status].toLowerCase()}.${next ? ` Next: ${caseLabel(next.caseNumber)} ${next.name}` : ' All cases in the queue are done.'}`);
    if (next) void navigate(`${runPath}/${next.id}`, { replace: true });
  };

  const askFinish = () => {
    if (!item || readOnly) return;
    if (!item.steps.length || suggested === 'untested' || untested.length > 0) {
      setFinishAsk({ untested: untested.length, suggested: suggestedStatus(item.steps, statusMap, true) });
      return;
    }
    finish(suggested, false);
  };

  const rerun = async () => {
    if (!item) return;
    try {
      flushText(item);
      await saver.drain();
      const v = await rerunRunItem(key, item.id, etagOf(revRef.current.get(item.id) ?? item.revision));
      const r = v.value;
      localRef.current.set(item.id, new Map());
      publish();
      revRef.current.set(item.id, r.revision);
      setOverrides((o) => new Map(o).set(item.id, r.status));
      qc.setQueryData(tmk.runItem(key, item.id), v);
      void qc.invalidateQueries({ queryKey: [...tmk.runItem(key, item.id), 'executions'] });
      setStepIdx(0);
      setAnnounce(`${caseLabel(item.caseNumber)}: run ${String(r.executionNo)} started`);
    } catch (e) {
      if (isConflict(e)) {
        void itemQ.refetch();
        notifications.show({ color: 'orange', title: 'Not restarted', message: 'The case changed meanwhile and was reloaded; check it and press Run again.' });
        return;
      }
      notifications.show({ color: 'red', title: 'Not restarted', message: errorMessage(e) });
    }
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      const typing = isTypingTarget(e.target);
      const a = runnerAction(e, { singleKeys, typing });
      if (!a) return;
      if (document.querySelector('[role="dialog"]')) return;
      if (item && item.iterationCount > 0 && a.type !== 'nextCase' && a.type !== 'prevCase' && a.type !== 'help') return;
      if (e.target instanceof HTMLElement && !typing && (e.target.tagName === 'BUTTON' || e.target.tagName === 'A') && a.type === 'enter') return;
      const cur = flat[stepIdx];
      e.preventDefault();
      switch (a.type) {
        case 'escape':
          if (item) flushText(item);
          if (typing && e.target instanceof HTMLElement) {
            e.target.blur();
            focusAfter.current = 'step';
            publish();
          }
          break;
        case 'nextStep':
        case 'enter':
          setStepIdx((i) => Math.min(flat.length - 1, i + 1));
          focusAfter.current = 'step';
          break;
        case 'prevStep':
          setStepIdx((i) => Math.max(0, i - 1));
          focusAfter.current = 'step';
          break;
        case 'status':
          if (cur) setStep(cur.path, a.status);
          break;
        case 'comment':
          commentRef.current?.focus();
          break;
        case 'attach':
          fileRef.current?.click();
          break;
        case 'passRest':
          if (item && !readOnly) {
            const rest = untestedLeaves(item.steps, statusMap);
            for (const p of rest) {
              setLocal(item, p, { status: 'passed' });
              enqueueStep(item, p);
            }
            setAnnounce(`${String(rest.length)} remaining steps passed`);
            setStepIdx(Math.max(0, flat.length - 1));
          }
          break;
        case 'nextCase':
          goto(1);
          break;
        case 'prevCase':
          goto(-1);
          break;
        case 'defect':
          setDefect(true);
          break;
        case 'finish':
          askFinish();
          break;
        case 'help':
          setHelp(true);
          break;
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  const queueView = (
    <RunQueue project={key} launchId={id} items={queue} currentId={itemId ?? null} statusOf={statusOf} mine={mine} onMine={setMine} filter={filter} onFilter={setFilter} onPick={() => setQueueDrawer(false)} />
  );
  const cur = flat[stepIdx];
  const fieldsProps = { readOnly, onText: (field: 'actual' | 'comment', v: string) => cur && setText(cur.path, field, v), onFiles: (f: FileList | null) => cur && void addFiles(cur.path, f), actualRef, commentRef, fileRef, uploading };

  return (
    <>
      <LiveRegion message={announce} />
      <SharedRefresh projectKey={key} launchId={id} />
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: l ? `#${String(l.number)} ${l.name}` : 'Launch', to: `/p/${key}/launches/${id}?tab=manual` }, { label: 'Manual run' }]}
        title="Manual run"
        badges={
          l?.manual && (
            <Badge variant="light" radius="sm" data-testid="run-progress">
              {l.manual.total - l.manual.untested - l.manual.inProgress} / {l.manual.total} done
            </Badge>
          )
        }
        actions={
          <Group gap="xs">
            <SaveIndicator s={saveState} onRetry={() => saver.retryNow()} />
            {mobile && (
              <Button size="xs" variant="default" leftSection={<IconListDetails size={14} />} onClick={() => setQueueDrawer(true)} data-testid="open-queue">
                Queue
              </Button>
            )}
            <Button size="xs" variant="default" leftSection={<IconKeyboard size={14} />} onClick={() => setHelp(true)} data-testid="open-help">
              Keys {singleKeys && <Kbd size="xs" ml={4}>?</Kbd>}
            </Button>
          </Group>
        }
      />
      {closed && (
        <Alert color="gray" variant="light" mb="sm" data-testid="run-closed">
          The manual run is closed; reopen it from the launch's Manual tab to record more results.
        </Alert>
      )}
      <QueryState query={all}>
        {() =>
          items.length === 0 ? (
            <EmptyState title="No manual cases in this launch" description="Start a launch from a test plan or add cases to it." />
          ) : (
            <div className="fp-tm-runner">
              {!mobile && <aside className="fp-tm-runner-queue">{queueView}</aside>}
              <section className="fp-tm-runner-main" aria-label="Current case">
                {!itemId || itemQ.isPending ? (
                  <Loader size="sm" />
                ) : itemQ.isError || !item ? (
                  <EmptyState title="Run item not found" description={errorMessage(itemQ.error)} />
                ) : (
                  <Stack gap="sm">
                    <Card withBorder radius="lg" padding="md">
                      <Stack gap="xs">
                        <Group justify="space-between" gap="xs" wrap="wrap">
                          <Group gap="xs" wrap="wrap">
                            <Text size="sm" c="dimmed" className="fp-mono">
                              {caseLabel(item.caseNumber)} · v{item.caseVersion}
                              {item.executionNo > 1 ? ` · run ${String(item.executionNo)}` : ''}
                            </Text>
                            <RunStatusBadge status={itemStatus} />
                          </Group>
                          <Group gap="xs">
                            {since > 0 && <Elapsed since={since} />}
                            <Button size="compact-xs" variant="subtle" component={Link} to={`/p/${key}/cases/${String(item.caseNumber)}`}>
                              Open case
                            </Button>
                          </Group>
                        </Group>
                        <h2 ref={titleRef} tabIndex={-1} className="fp-tm-run-title" data-testid="run-case-title">
                          {item.name}
                        </h2>
                        {item.folderPath.length > 0 && (
                          <Text size="xs" c="dimmed">
                            {item.folderPath.join(' / ')}
                          </Text>
                        )}
                        {item.preconditionsHtml && item.iterationCount === 0 && (
                          <Stack gap={2}>
                            <Text size="xs" fw={600} c="dimmed">
                              Preconditions
                            </Text>
                            <CaseHtml html={item.preconditionsHtml} project={key} />
                          </Stack>
                        )}
                      </Stack>
                    </Card>
                    {item.iterationCount > 0 ? (
                      <Iterations projectKey={key} launchId={id} item={item} readOnly={readOnly} onAnnounce={setAnnounce} />
                    ) : mobile && cur ? (
                      <Card withBorder radius="lg" padding="md" data-testid="mobile-step" tabIndex={-1} data-path={cur.path} data-status={effective.get(cur.path) ?? 'untested'}>
                        <Stack gap="xs">
                          <Group justify="space-between">
                            <Text size="sm" fw={600}>
                              Step {cur.path} · {stepIdx + 1} of {flat.length}
                            </Text>
                            <RunStatusBadge status={effective.get(cur.path) ?? 'untested'} size="xs" />
                          </Group>
                          {cur.step.shared ? <SharedHead item={item} step={cur.step} /> : <CaseHtml html={htmlFor(item, cur.path)?.action ?? ''} project={key} />}
                          {cur.step.expected && (
                            <div className="fp-tm-expected">
                              <Text span size="xs" c="dimmed">
                                Expected:{' '}
                              </Text>
                              <CaseHtml html={htmlFor(item, cur.path)?.expected ?? ''} project={key} />
                            </div>
                          )}
                          {cur.step.data && (
                            <div className="fp-tm-expected">
                              <Text span size="xs" c="dimmed">
                                Data:{' '}
                              </Text>
                              <CaseHtml html={htmlFor(item, cur.path)?.data ?? ''} project={key} />
                            </div>
                          )}
                          <StepFields {...fieldsProps} project={key} path={cur.path} local={local.get(cur.path) ?? emptyLocal()} />
                          <Group grow>
                            <Button variant="default" h={44} leftSection={<IconArrowLeft size={16} />} disabled={stepIdx === 0} onClick={() => setStepIdx((i) => Math.max(0, i - 1))}>
                              Previous
                            </Button>
                            <Button variant="default" h={44} rightSection={<IconArrowRight size={16} />} disabled={stepIdx >= flat.length - 1} onClick={() => setStepIdx((i) => Math.min(flat.length - 1, i + 1))}>
                              Next step
                            </Button>
                          </Group>
                        </Stack>
                      </Card>
                    ) : (
                      !mobile && <RunSteps project={key} item={item} flat={flat} effective={effective} local={local} current={stepIdx} onCurrent={setStepIdx} onSet={(p, s) => setStep(p, s, false)} fields={fieldsProps} />
                    )}
                    {item.iterationCount === 0 && (
                    <Card withBorder radius="lg" padding="sm" className="fp-tm-finish" data-testid="finish-panel">
                      <Stack gap="xs">
                        <Group justify="space-between" gap="xs" wrap="wrap">
                          <Group gap={6}>
                            <Text size="sm">Suggested:</Text>
                            <span data-testid="suggested-status" data-status={suggested}>
                              <RunStatusBadge status={suggested} />
                            </span>
                            {untested.length > 0 && (
                              <Text size="xs" c="dimmed" data-testid="suggested-untested">
                                {untested.length} untested{skipUntested && FINAL.includes(suggestedStatus(item.steps, statusMap, true)) ? ` · finishing marks them skipped: ${RUN_STATUS_LABEL[suggestedStatus(item.steps, statusMap, true)].toLowerCase()}` : ''}
                              </Text>
                            )}
                          </Group>
                          <Group gap="xs">
                            <Tooltip label="Quick defect (d)">
                              <Button size="xs" variant="default" leftSection={<IconBug size={14} />} onClick={() => setDefect(true)} disabled={readOnly} data-testid="open-defect">
                                Defect
                              </Button>
                            </Tooltip>
                            {FINAL.includes(itemStatus) && (
                              <Button size="xs" variant="default" leftSection={<IconRotateClockwise size={14} />} onClick={() => void rerun()} disabled={readOnly} data-testid="rerun">
                                Run again
                              </Button>
                            )}
                            <Button size="xs" variant="subtle" leftSection={<IconPlayerTrackNext size={14} />} onClick={() => goto(1)}>
                              Next case (n)
                            </Button>
                          </Group>
                        </Group>
                        {untested.length > 0 && <Checkbox size="xs" label="Mark untested steps as skipped when finishing" checked={skipUntested} onChange={(e) => setSkipUntested(e.currentTarget.checked)} />}
                        <Group gap="xs" wrap="wrap">
                          {FINAL.map((s) => (
                            <Button
                              key={s}
                              size="xs"
                              h={mobile ? 44 : undefined}
                              variant={s === suggested ? 'filled' : 'light'}
                              color={s === 'passed' ? 'green' : s === 'failed' ? 'red' : s === 'blocked' ? 'orange' : 'gray'}
                              onClick={() => finish(s, untested.length > 0 && skipUntested)}
                              disabled={readOnly}
                              data-testid={`finish-${s}`}
                            >
                              Finish: {RUN_STATUS_LABEL[s]}
                            </Button>
                          ))}
                          <Text size="xs" c="dimmed" visibleFrom="sm">
                            <Kbd size="xs">Ctrl</Kbd>+<Kbd size="xs">Enter</Kbd> finishes with the suggestion
                          </Text>
                        </Group>
                      </Stack>
                    </Card>
                    )}
                    <Executions project={key} itemId={item.id} executionNo={item.executionNo} />
                    {mobile && cur && item.iterationCount === 0 && (
                      <div className="fp-tm-sticky-bar" data-testid="sticky-bar">
                        <StatusButtons path={cur.path} current={local.get(cur.path)?.status ?? 'untested'} readOnly={readOnly} onSet={(s) => setStep(cur.path, s)} size="lg" />
                      </div>
                    )}
                  </Stack>
                )}
              </section>
            </div>
          )
        }
      </QueryState>
      <Drawer opened={queueDrawer} onClose={() => setQueueDrawer(false)} title="Queue" position="left" size="85%">
        {queueView}
      </Drawer>
      <HelpModal
        opened={help}
        onClose={() => setHelp(false)}
        singleKeys={singleKeys}
        onSingleKeys={(on) => {
          setSingleKeysEnabled(on);
          setSingleKeys(on);
        }}
      />
      {item && defect && (
        <DefectDrawer
          project={key}
          item={item}
          local={local}
          opened={defect}
          onClose={() => setDefect(false)}
          beforeIssue={async () => {
            flushText(item);
            await saver.drain();
          }}
          final={FINAL.includes(itemStatus)}
          finishFailed={async () => {
            finish('failed', untested.length > 0, false);
            await saver.drain();
          }}
        />
      )}
      <Modal opened={!!finishAsk} onClose={() => setFinishAsk(null)} title="Finish the case" attributes={{ content: { 'data-testid': 'finish-ask' } }}>
        {finishAsk && (
          <Stack gap="sm">
            {finishAsk.untested > 0 ? (
              <Text size="sm">
                {finishAsk.untested} step{finishAsk.untested === 1 ? ' is' : 's are'} untested. Mark {finishAsk.untested === 1 ? 'it' : 'them'} as skipped and finish as <b>{RUN_STATUS_LABEL[finishAsk.suggested]}</b>?
              </Text>
            ) : (
              <Text size="sm">Choose the status of this case.</Text>
            )}
            <Group justify="flex-end" gap="xs" wrap="wrap">
              <Button variant="default" onClick={() => setFinishAsk(null)}>
                Cancel
              </Button>
              {finishAsk.untested > 0 && FINAL.includes(finishAsk.suggested) ? (
                <Button data-autofocus onClick={() => finish(finishAsk.suggested, true)} data-testid="finish-ask-confirm">
                  Skip them and finish
                </Button>
              ) : (
                FINAL.map((s) => (
                  <Button key={s} variant="light" onClick={() => finish(s, finishAsk.untested > 0)} data-testid={`finish-ask-${s}`}>
                    {RUN_STATUS_LABEL[s]}
                  </Button>
                ))
              )}
            </Group>
          </Stack>
        )}
      </Modal>
      {item && saveState.state === 'retrying' && (
        <Button className="fp-no-print" size="xs" variant="light" color="orange" leftSection={<IconRefresh size={14} />} onClick={() => saver.retryNow()} pos="fixed" bottom={mobile ? 80 : 16} right={16}>
          Retry saving
        </Button>
      )}
    </>
  );
}
