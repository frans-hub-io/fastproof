import { Button, Drawer, Group, Select, Stack, Text, Textarea, TextInput } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconExternalLink } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { errorMessage } from '../../api/client';
import { useDefectTypes, useSettings } from '../../api/stageC';
import { fetchNewIssueUrl, useRunItemDefect, type RunItem } from '../../api/tm';
import { GROUP_LABEL } from '../../lib/defects';
import { DEFECT_GROUPS } from '../../api/types-next';
import { caseLabel } from '../../lib/tm/filters';
import { flattenSteps } from '../../lib/tm/steps';
import type { LocalStep } from './types';

function failingText(item: RunItem, local: ReadonlyMap<string, LocalStep>): string {
  const flat = flattenSteps(item.steps);
  const bad = flat.find((f) => {
    const s = local.get(f.path)?.status;
    return s === 'failed' || s === 'blocked';
  });
  if (!bad) return '';
  const r = local.get(bad.path);
  return [`Step ${bad.path} ${r?.status ?? ''}: ${bad.step.action}`, bad.step.expected ? `Expected: ${bad.step.expected}` : '', r?.actual ? `Actual: ${r.actual}` : ''].filter(Boolean).join('\n');
}

export function DefectDrawer({ project, item, local, opened, onClose, beforeIssue, final = true, finishFailed }: { project: string; item: RunItem; local: ReadonlyMap<string, LocalStep>; opened: boolean; onClose: () => void; beforeIssue?: () => Promise<void>; final?: boolean; finishFailed?: () => Promise<void> }) {
  const types = useDefectTypes(project);
  const settings = useSettings(project);
  const defect = useRunItemDefect(project, item.id);
  const [typeId, setTypeId] = useState<string | null>(null);
  const [comment, setComment] = useState(() => failingText(item, local));
  const [issue, setIssue] = useState('');
  const [opening, setOpening] = useState(false);
  const tracker = settings.data?.issueTrackers[0];
  const grouped = useMemo(
    () => DEFECT_GROUPS.map((g) => ({ group: GROUP_LABEL[g], items: (types.data ?? []).filter((t) => t.group === g).map((t) => ({ value: t.id, label: t.name })) })).filter((g) => g.items.length > 0),
    [types.data],
  );
  const issueText = issue.trim();
  const isUrl = /^https?:\/\//i.test(issueText);

  const openNewIssue = async () => {
    setOpening(true);
    try {
      await beforeIssue?.();
      const url = await fetchNewIssueUrl(project, item.id, tracker?.type);
      if (/^https?:\/\//i.test(url)) window.open(url, '_blank', 'noopener,noreferrer');
    } catch (e) {
      notifications.show({ color: 'red', title: 'No issue link', message: errorMessage(e) });
    } finally {
      setOpening(false);
    }
  };

  const [finishing, setFinishing] = useState(false);
  const save = async () => {
    if (!final && finishFailed) {
      setFinishing(true);
      try {
        await finishFailed();
      } finally {
        setFinishing(false);
      }
    }
    send();
  };
  const send = () =>
    defect.mutate(
      { ...(typeId ? { defectTypeId: typeId } : {}), ...(comment.trim() ? { comment: comment.trim() } : {}), ...(issueText ? { issue: isUrl ? { url: issueText } : { url: tracker ? tracker.urlTemplate.replace('{key}', encodeURIComponent(issueText)) : issueText, key: issueText } } : {}) },
      {
        onSuccess: () => {
          notifications.show({ color: 'green', title: 'Defect saved', message: caseLabel(item.caseNumber) });
          onClose();
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Defect not saved', message: errorMessage(e) }),
      },
    );

  return (
    <Drawer opened={opened} onClose={onClose} position="right" size="md" title={`Defect for ${caseLabel(item.caseNumber)}`} attributes={{ content: { 'data-testid': 'defect-drawer' } }}>
      <Stack gap="sm">
        <Select label="Defect type" data={grouped} value={typeId} onChange={setTypeId} searchable clearable comboboxProps={{ withinPortal: true }} data-testid="defect-type" />
        <Textarea label="Comment" value={comment} onChange={(e) => setComment(e.currentTarget.value)} autosize minRows={4} maxLength={4000} data-autofocus data-testid="defect-comment" />
        <TextInput label="Issue" placeholder={tracker ? `Key like ${tracker.keyRegex} or a link` : 'https://…'} value={issue} onChange={(e) => setIssue(e.currentTarget.value)} data-testid="defect-issue" />
        <Button variant="light" leftSection={<IconExternalLink size={14} />} onClick={() => void openNewIssue()} loading={opening} disabled={!tracker} data-testid="defect-new-issue">
          Open a new issue{tracker ? ` in ${tracker.type === 'gitlab' ? 'GitLab' : tracker.type === 'github' ? 'GitHub' : 'Jira'}` : ''}
        </Button>
        {!final && (
          <Text size="xs" c="dimmed" data-testid="defect-finish-note">
            A defect belongs to the case's result, so saving finishes the case as Failed first.
          </Text>
        )}
        <Text size="xs" c="dimmed">
          {tracker ? '' : 'No issue tracker is configured (Project settings → Integrations). '}The new issue opens in the tracker with the step, the expected and the actual result and a link back here; paste its key above.
        </Text>
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={() => void save()} loading={defect.isPending || finishing} data-testid="defect-save">
            {final ? 'Save' : 'Finish as failed and save'}
          </Button>
        </Group>
      </Stack>
    </Drawer>
  );
}
