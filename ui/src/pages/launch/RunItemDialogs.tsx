import { Alert, Button, Group, Modal, MultiSelect, Progress, Radio, Stack } from '@mantine/core';
import { useDebouncedValue } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { useQueryClient } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import { errorMessage } from '../../api/client';
import { invalidateRun, patchRunItems, useAddRunItems, useCases, type RunItemBulkResult, type RunItemPatch, type RunItemsAdded, type RunStatus } from '../../api/tm';
import { caseLabel } from '../../lib/tm/filters';
import { RUN_STATUS_LABEL } from '../../lib/tm/steps';
import { PersonInput } from '../cases/PersonInput';

const RESULTS: RunStatus[] = ['passed', 'failed', 'blocked', 'skipped'];

function skippedText(r: RunItemsAdded): string {
  if (!r.skipped) return '';
  const skips = r.skips ?? [];
  const count = (code: string) => skips.filter((x) => x.code === code).length;
  const parts = [count('automated') ? `${plural(count('automated'), 'automated case')} (they run in CI)` : '', count('in_launch') ? `${plural(count('in_launch'), 'case')} already in the run` : ''].filter(Boolean);
  return `${plural(r.skipped, 'case')} skipped${parts.length ? `: ${parts.join(', ')}` : ''}`;
}

function plural(n: number, word: string): string {
  return `${String(n)} ${word}${n === 1 ? '' : 's'}`;
}

/** Runs one patch over the chosen items with a progress bar; reports partial failures. */
function useBulkPatch(project: string, launchId: string) {
  const qc = useQueryClient();
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [result, setResult] = useState<RunItemBulkResult | null>(null);
  const run = async (ids: readonly string[], patch: RunItemPatch, done: string): Promise<boolean> => {
    setProgress({ done: 0, total: ids.length });
    setResult(null);
    const r = await patchRunItems(project, ids, patch, (n) => setProgress({ done: n, total: ids.length }));
    setProgress(null);
    invalidateRun(qc, project, launchId);
    if (r.failed.length === 0) {
      notifications.show({ color: 'green', title: done, message: plural(r.done, 'case') });
      return true;
    }
    setResult(r);
    return false;
  };
  return { run, progress, result };
}

function BulkState({ progress, result }: { progress: { done: number; total: number } | null; result: RunItemBulkResult | null }) {
  return (
    <>
      {progress && <Progress value={(progress.done / Math.max(1, progress.total)) * 100} aria-label={`${String(progress.done)} of ${String(progress.total)} done`} animated />}
      {result && result.failed.length > 0 && (
        <Alert color="red" variant="light" title={`${plural(result.failed.length, 'case')} not changed`} data-testid="bulk-failed">
          {result.done > 0 ? `${plural(result.done, 'case')} changed. ` : ''}
          {[...new Set(result.failed.map((f) => f.message))].slice(0, 3).join(' · ')}
        </Alert>
      )}
    </>
  );
}

/** Assigns one or many run items to a person (or unassigns them). */
export function AssignDialog({ project, launchId, ids, current, onClose }: { project: string; launchId: string; ids: string[]; current?: string | null; onClose: (done?: boolean) => void }) {
  const [who, setWho] = useState(current ?? '');
  const bulk = useBulkPatch(project, launchId);
  const name = who.trim();
  const submit = async () => {
    if (await bulk.run(ids, { assignee: name || null }, name ? `Assigned to ${name}` : 'Unassigned')) onClose(true);
  };
  return (
    <Modal opened onClose={() => onClose()} title={ids.length === 1 ? 'Assign the case' : `Assign ${plural(ids.length, 'case')}`} attributes={{ content: { 'data-testid': 'assign-dialog' } }}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Stack gap="sm">
          <PersonInput project={project} label="Assignee" placeholder="Leave empty to unassign" value={who} onChange={setWho} data-autofocus data-testid="assign-input" />
          <BulkState progress={bulk.progress} result={bulk.result} />
          <Group justify="flex-end">
            <Button variant="default" onClick={() => onClose()}>
              Cancel
            </Button>
            <Button type="submit" loading={!!bulk.progress} data-testid="assign-confirm">
              {name ? `Assign to ${name}` : 'Unassign'}
            </Button>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}

/** Finishes many run items with one status after a confirmation; their untested steps become skipped. */
export function SetResultDialog({ project, launchId, ids, finished, onClose }: { project: string; launchId: string; ids: string[]; finished: number; onClose: (done?: boolean) => void }) {
  const [status, setStatus] = useState<RunStatus | null>(null);
  const bulk = useBulkPatch(project, launchId);
  const submit = async () => {
    if (!status) return;
    if (await bulk.run(ids, { status, untestedStepsTo: 'skipped' }, `Set to ${RUN_STATUS_LABEL[status].toLowerCase()}`)) onClose(true);
  };
  return (
    <Modal opened onClose={() => onClose()} title={`Set the result of ${plural(ids.length, 'case')}`} attributes={{ content: { 'data-testid': 'set-result-dialog' } }}>
      <Stack gap="sm">
        <Radio.Group value={status} onChange={(v) => setStatus(RESULTS.find((s) => s === v) ?? null)} label="Result" required>
          <Group gap="md" mt={6}>
            {RESULTS.map((s) => (
              <Radio key={s} value={s} label={RUN_STATUS_LABEL[s]} data-testid={`set-result-${s}`} />
            ))}
          </Group>
        </Radio.Group>
        {status && (
          <Alert color={status === 'passed' ? 'green' : status === 'failed' ? 'red' : status === 'blocked' ? 'orange' : 'gray'} variant="light" data-testid="set-result-summary">
            {plural(ids.length, 'case')} will be finished as <b>{RUN_STATUS_LABEL[status]}</b>; their untested steps become skipped and you are recorded as the executor.
            {finished > 0 ? ` ${plural(finished, 'case')} already finished get the new status too.` : ''}
          </Alert>
        )}
        <BulkState progress={bulk.progress} result={bulk.result} />
        <Group justify="flex-end">
          <Button variant="default" onClick={() => onClose()}>
            Cancel
          </Button>
          <Button onClick={() => void submit()} disabled={!status} loading={!!bulk.progress} data-testid="set-result-confirm">
            {status ? `Set ${plural(ids.length, 'case')} to ${RUN_STATUS_LABEL[status]}` : 'Choose a result'}
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}

/** Adds manual cases to a running launch (`POST /launches/{id}/run-items`). */
export function AddCasesDialog({ project, launchId, inRun, onClose }: { project: string; launchId: string; inRun: ReadonlySet<number>; onClose: () => void }) {
  const add = useAddRunItems(project, launchId);
  const [search, setSearch] = useState('');
  const [q] = useDebouncedValue(search.trim(), 250);
  const [chosen, setChosen] = useState<string[]>([]);
  const [labels, setLabels] = useState<Map<string, string>>(() => new Map());
  const [who, setWho] = useState('');
  const cases = useCases(project, { q: q || undefined, automation: 'manual' });
  const data = useMemo(() => {
    const out = new Map<string, string>();
    for (const v of chosen) out.set(v, labels.get(v) ?? v);
    for (const c of cases.data?.pages.flatMap((p) => p.items) ?? []) {
      if (inRun.has(c.number) || out.has(String(c.number))) continue;
      out.set(String(c.number), `${caseLabel(c.number)} ${c.name}`);
    }
    return [...out.entries()].map(([value, label]) => ({ value, label }));
  }, [cases.data, chosen, labels, inRun]);
  const submit = () =>
    add.mutate(
      { caseNumbers: chosen.map(Number), assignee: who.trim() || null },
      {
        onSuccess: (r) => {
          notifications.show({ color: r.added ? 'green' : 'yellow', title: r.added ? `Added ${plural(r.added, 'case')}` : 'Nothing added', message: skippedText(r) });
          onClose();
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Cases not added', message: errorMessage(e) }),
      },
    );
  return (
    <Modal opened onClose={onClose} title="Add cases to the run" size="lg" attributes={{ content: { 'data-testid': 'add-cases-dialog' } }}>
      <Stack gap="sm">
        <MultiSelect
          label="Manual cases"
          description="Search by number or name; automated cases run in CI and are not added"
          placeholder={chosen.length ? '' : 'TC-12 or a name'}
          data={data}
          value={chosen}
          onChange={(v) => {
            setLabels((m) => {
              const next = new Map(m);
              for (const d of data) if (v.includes(d.value)) next.set(d.value, d.label);
              return next;
            });
            setChosen(v);
          }}
          searchable
          searchValue={search}
          onSearchChange={setSearch}
          filter={({ options }) => options}
          nothingFoundMessage={cases.isFetching ? 'Searching…' : 'No manual case matches'}
          maxValues={500}
          comboboxProps={{ withinPortal: true }}
          data-autofocus
          data-testid="add-cases-input"
        />
        <PersonInput project={project} label="Assignee (optional)" placeholder="Unassigned" value={who} onChange={setWho} data-testid="add-cases-assignee" />
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={submit} disabled={!chosen.length} loading={add.isPending} data-testid="add-cases-confirm">
            {chosen.length ? `Add ${plural(chosen.length, 'case')}` : 'Add'}
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}
