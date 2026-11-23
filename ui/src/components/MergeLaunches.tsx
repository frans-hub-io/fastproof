import { Alert, Anchor, Button, Checkbox, Group, Modal, Radio, Select, Stack, Text, TextInput } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconAlertTriangle, IconArrowMerge } from '@tabler/icons-react';
import { useDebouncedValue } from '@mantine/hooks';
import { useMemo, useState } from 'react';
import { Link, useNavigate } from 'react-router';
import { errorMessage } from '../api/client';
import { useLaunchMerges, useMergeLaunches, useMergePreview, useUndoMerge, type LaunchMerge } from '../api/merges';
import type { Launch } from '../api/types';
import { formatRelative } from '../lib/format';

function untilText(iso: string | null | undefined): string {
  if (!iso) return '';
  return new Date(iso).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
}

function PreviewText({ preview }: { preview: LaunchMerge }) {
  const c = preview.counts;
  return (
    <Text size="sm" data-testid="merge-preview">
      {c.attempts} attempts of {c.results} tests from {preview.sources.length} launch{preview.sources.length === 1 ? '' : 'es'}.{' '}
      {c.sharedSeries > 0
        ? `${String(c.sharedSeries)} test${c.sharedSeries === 1 ? ' was' : 's were'} run in more than one of them: the later run becomes a new attempt and the result, and different outcomes mark the test flaky in the launch.`
        : 'No test was run in more than one of them.'}
    </Text>
  );
}

/** Merges the selected launches into a new launch or into one of them. */
export function MergeLaunchesModal({ project, launches, opened, onClose, onMerged }: { project: string; launches: Launch[]; opened: boolean; onClose: () => void; onMerged: () => void }) {
  const navigate = useNavigate();
  const ordered = useMemo(() => [...launches].sort((a, b) => a.number - b.number), [launches]);
  const [mode, setMode] = useState<'new' | 'into'>('new');
  const [into, setInto] = useState<string | null>(ordered[0]?.id ?? null);
  const [name, setName] = useState(ordered[0]?.name ?? '');
  const [final, setFinal] = useState(false);
  const body = useMemo(
    () => ({
      launchIds: ordered.map((l) => l.id),
      into: mode === 'into' ? into : null,
      name: mode === 'new' && name.trim() ? name.trim() : null,
      final,
    }),
    [ordered, mode, into, name, final],
  );
  const previewBody = useMemo(() => ({ launchIds: body.launchIds, into: body.into, final: body.final }), [body.launchIds, body.into, body.final]);
  const [debouncedPreview] = useDebouncedValue(previewBody, 300);
  const preview = useMergePreview(project, opened ? debouncedPreview : null);
  const merge = useMergeLaunches(project);
  const nameError = mode === 'new' && name.length > 300 ? 'At most 300 characters' : undefined;

  const submit = () =>
    merge.mutate(body, {
      onSuccess: (m) => {
        notifications.show({
          color: 'green',
          title: `Merged into #${String(m.targetNumber ?? '')}`,
          message: m.status === 'final' ? 'The merged launches are deleted.' : `You can undo the merge on the launch page until ${untilText(m.undoUntil)}.`,
        });
        onMerged();
        onClose();
        if (m.targetId) void navigate(`/p/${project}/launches/${m.targetId}`);
      },
    });

  return (
    <Modal opened={opened} onClose={onClose} title={`Merge ${String(ordered.length)} launches`} size="lg" data-testid="merge-modal">
      <Stack gap="sm">
        <Text size="sm" c="dimmed">
          {ordered.map((l) => `#${String(l.number)}`).join(', ')}
        </Text>
        <Radio.Group value={mode} onChange={(v) => setMode(v === 'into' ? 'into' : 'new')} label="Merge into">
          <Stack gap={6} mt={6}>
            <Radio value="new" label="A new launch" data-testid="merge-mode-new" />
            <Radio value="into" label="One of the selected launches (a re-run joins the original)" data-testid="merge-mode-into" />
          </Stack>
        </Radio.Group>
        {mode === 'new' ? (
          <TextInput label="Name of the new launch" value={name} onChange={(e) => setName(e.currentTarget.value)} error={nameError} data-testid="merge-name" />
        ) : (
          <Select
            label="Target launch"
            data={ordered.map((l) => ({ value: l.id, label: `#${String(l.number)} ${l.name}` }))}
            value={into}
            onChange={setInto}
            allowDeselect={false}
            data-testid="merge-into"
          />
        )}
        {preview.isPending && preview.fetchStatus !== 'idle' && (
          <Text size="sm" c="dimmed">
            Checking the launches…
          </Text>
        )}
        {preview.isError && (
          <Alert color="red" icon={<IconAlertTriangle size={16} />} data-testid="merge-refused">
            {errorMessage(preview.error)}
          </Alert>
        )}
        {preview.data && <PreviewText preview={preview.data} />}
        <Checkbox
          checked={final}
          onChange={(e) => setFinal(e.currentTarget.checked)}
          label="Delete the merged launches now (cannot be undone)"
          description="Needs the permission to delete launches. Without it, the launches stay hidden and the merge can be undone for a while."
          data-testid="merge-final"
        />
        {final ? (
          <Alert color="orange" icon={<IconAlertTriangle size={16} />}>
            The {mode === 'into' ? 'other launches' : 'selected launches'} are deleted within a few minutes of the merge (writes still on their way are copied first). This cannot be undone.
          </Alert>
        ) : (
          preview.data?.undoUntil && (
            <Text size="sm" c="dimmed">
              The merged launches are hidden, and you can undo the merge until {untilText(preview.data.undoUntil)}.
            </Text>
          )
        )}
        {merge.isError && (
          <Alert color="red" icon={<IconAlertTriangle size={16} />}>
            {errorMessage(merge.error)}
          </Alert>
        )}
        <Group justify="flex-end" gap="xs">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
          <Button
            color={final ? 'red' : undefined}
            leftSection={<IconArrowMerge size={16} />}
            onClick={submit}
            loading={merge.isPending}
            disabled={!preview.data || !!nameError || (mode === 'into' && !into)}
            data-testid="merge-submit"
          >
            {final ? 'Merge and delete' : 'Merge'}
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}

/** On a launch page: where the launch was merged from or into, with undo while it is possible. */
export function MergeBanner({ project, launch, canUndo }: { project: string; launch: Launch; canUndo: boolean }) {
  const merges = useLaunchMerges(project, launch.id);
  const undo = useUndoMerge(project);
  const navigate = useNavigate();
  const active = (merges.data ?? []).filter((m) => m.status === 'active' || (m.targetId === launch.id && m.status === 'final'));
  if (launch.mergedInto) {
    const m = (merges.data ?? []).find((x) => x.targetId === launch.mergedInto);
    return (
      <Alert color="gray" icon={<IconArrowMerge size={16} />} data-testid="merged-into">
        <Group justify="space-between" gap="xs">
          <Text size="sm">
            This launch was merged into{' '}
            <Anchor component={Link} to={`/p/${project}/launches/${launch.mergedInto}`}>
              {m?.targetNumber ? `#${String(m.targetNumber)}` : 'another launch'}
            </Anchor>
            ; it is hidden from the list and takes no more results.
          </Text>
          {canUndo && m?.status === 'active' && m.id && (
            <Button size="xs" variant="default" loading={undo.isPending} onClick={() => m.id && undo.mutate(m.id)}>
              Undo merge
            </Button>
          )}
        </Group>
      </Alert>
    );
  }
  const mine = active.filter((m) => m.targetId === launch.id);
  if (mine.length === 0) return null;
  return (
    <Stack gap={6}>
      {mine.map((m) => (
        <Alert key={m.id} color="blue" icon={<IconArrowMerge size={16} />} data-testid="merged-from">
          <Group justify="space-between" gap="xs">
            <Text size="sm">
              Merged from {m.sources.map((s) => (s.number ? `#${String(s.number)}` : 'a deleted launch')).join(', ')}
              {m.createdBy ? ` by ${m.createdBy}` : ''}
              {m.createdAt ? ` ${formatRelative(m.createdAt)}` : ''}.{' '}
              {m.analysing ? 'History and statistics are being updated. ' : ''}
              {m.status === 'active' && m.undoUntil ? `Undo is possible until ${untilText(m.undoUntil)}.` : 'The merged launches are deleted.'}
            </Text>
            {canUndo && m.status === 'active' && m.id && (
              <Button
                size="xs"
                variant="default"
                loading={undo.isPending}
                onClick={() =>
                  m.id &&
                  undo.mutate(m.id, {
                    onError: (e) => notifications.show({ color: 'red', title: 'Undo failed', message: errorMessage(e) }),
                    onSuccess: (r) => {
                      notifications.show({ color: 'green', message: 'Merge undone: the launches are back.' });
                      if (r.createdTarget) void navigate(`/p/${project}`);
                    },
                  })
                }
                data-testid="undo-merge"
              >
                Undo merge
              </Button>
            )}
          </Group>
        </Alert>
      ))}
    </Stack>
  );
}
