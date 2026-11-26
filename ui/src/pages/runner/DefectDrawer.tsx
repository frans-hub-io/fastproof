import { Anchor, Button, Card, CopyButton, Drawer, Group, Select, Stack, Text, Textarea, TextInput } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconCheck, IconCopy, IconExternalLink } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { errorMessage } from '../../api/client';
import { useDefectTypes, useSettings } from '../../api/stageC';
import { fetchNewIssue, useRunItemDefect, type RunItem } from '../../api/tm';
import { GROUP_LABEL } from '../../lib/defects';
import { DEFECT_GROUPS } from '../../api/types-next';
import type { RunIteration } from '../../api/parameters';
import { caseLabel } from '../../lib/tm/filters';
import { newBudget, substitute, valuesLabel } from '../../lib/tm/parameters';
import { flattenSteps } from '../../lib/tm/steps';
import type { LocalStep } from './types';

const LISTED_ITERATIONS = 10;

/** The iteration, its values and the first failed or blocked step with the values put in. */
function iterationText(item: RunItem, it: RunIteration): string {
  const budget = newBudget(32 * 1024);
  const head = `Iteration #${String(it.position)}: ${valuesLabel(item.parameterColumns, it.values)} (${it.status})`;
  const flat = flattenSteps(item.steps);
  const bad = it.stepResults.find((r) => r.status === 'failed' || r.status === 'blocked');
  const step = bad ? flat.find((f) => f.path === bad.path) : undefined;
  if (!bad || !step) return head;
  return [head, `Step ${bad.path} ${bad.status}: ${substitute(step.step.action, it.values, budget)}`, step.step.expected ? `Expected: ${substitute(step.step.expected, it.values, budget)}` : '', bad.actual ? `Actual: ${bad.actual}` : ''].filter(Boolean).join('\n');
}

function itemIterationsText(item: RunItem): string {
  const bad = item.iterations.filter((i) => i.status === 'failed' || i.status === 'blocked');
  const listed = bad.slice(0, LISTED_ITERATIONS).map((i) => iterationText(item, i));
  if (bad.length > LISTED_ITERATIONS) listed.push(`… and ${String(bad.length - LISTED_ITERATIONS)} more`);
  return [`${String(bad.length)} of ${String(item.iterationCount)} iterations not passed`, ...listed].join('\n\n');
}

function failingText(item: RunItem, local: ReadonlyMap<string, LocalStep>, iteration?: RunIteration): string {
  if (iteration) return iterationText(item, iteration);
  if (item.iterationCount > 0) return itemIterationsText(item);
  const flat = flattenSteps(item.steps);
  const bad = flat.find((f) => {
    const s = local.get(f.path)?.status;
    return s === 'failed' || s === 'blocked';
  });
  if (!bad) return '';
  const r = local.get(bad.path);
  return [`Step ${bad.path} ${r?.status ?? ''}: ${bad.step.action}`, bad.step.expected ? `Expected: ${bad.step.expected}` : '', r?.actual ? `Actual: ${r.actual}` : ''].filter(Boolean).join('\n');
}

/**
 * Creates or links a defect of a run item. For a parameterized item it is about one iteration (`iteration`: its values
 * and failing step are in the text, and the defect goes to that iteration's result) or, without it, about every failed
 * or blocked iteration of the item.
 */
export function DefectDrawer({ project, item, local, opened, onClose, beforeIssue, final = true, finishFailed, iteration }: { project: string; item: RunItem; local: ReadonlyMap<string, LocalStep>; iteration?: RunIteration; opened: boolean; onClose: () => void; beforeIssue?: () => Promise<void>; final?: boolean; finishFailed?: () => Promise<void> }) {
  const types = useDefectTypes(project);
  const settings = useSettings(project);
  const defect = useRunItemDefect(project, item.id);
  const [typeId, setTypeId] = useState<string | null>(null);
  const [comment, setComment] = useState(() => failingText(item, local, iteration));
  const [issue, setIssue] = useState('');
  const [opening, setOpening] = useState(false);
  const [prefill, setPrefill] = useState<{ url: string; text: string } | null>(null);
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
      const r = await fetchNewIssue(project, item.id, tracker?.type, iteration?.rowId);
      const url = /^https?:\/\//i.test(r.url) ? r.url : '';
      if (!r.prefilled && r.prefill) setPrefill({ url, text: `${r.prefill.title}\n\n${r.prefill.description}` });
      else if (url) window.open(url, '_blank', 'noopener,noreferrer');
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
      { ...(iteration ? { rowId: iteration.rowId } : {}), ...(typeId ? { defectTypeId: typeId } : {}), ...(comment.trim() ? { comment: comment.trim() } : {}), ...(issueText ? { issue: isUrl ? { url: issueText } : { url: tracker ? tracker.urlTemplate.replace('{key}', encodeURIComponent(issueText)) : issueText, key: issueText } } : {}) },
      {
        onSuccess: () => {
          notifications.show({ color: 'green', title: 'Defect saved', message: caseLabel(item.caseNumber) });
          onClose();
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Defect not saved', message: errorMessage(e) }),
      },
    );

  return (
    <Drawer opened={opened} onClose={onClose} closeButtonProps={{ 'aria-label': 'Close' }} position="right" size="md" title={iteration ? `Defect for ${caseLabel(item.caseNumber)}, iteration #${String(iteration.position)}` : item.iterationCount > 0 ? `Defect for ${caseLabel(item.caseNumber)}, failed iterations` : `Defect for ${caseLabel(item.caseNumber)}`} attributes={{ content: { 'data-testid': 'defect-drawer' } }}>
      <Stack gap="sm">
        <Select label="Defect type" data={grouped} value={typeId} onChange={setTypeId} searchable clearable comboboxProps={{ withinPortal: true }} data-testid="defect-type" />
        <Textarea label="Comment" value={comment} onChange={(e) => setComment(e.currentTarget.value)} autosize minRows={4} maxLength={4000} data-autofocus data-testid="defect-comment" />
        <TextInput label="Issue" placeholder={tracker ? `Key like ${tracker.keyRegex} or a link` : 'https://…'} value={issue} onChange={(e) => setIssue(e.currentTarget.value)} data-testid="defect-issue" />
        <Button variant="light" leftSection={<IconExternalLink size={14} />} onClick={() => void openNewIssue()} loading={opening} disabled={!tracker} data-testid="defect-new-issue">
          Open a new issue{tracker ? ` in ${tracker.type === 'gitlab' ? 'GitLab' : tracker.type === 'github' ? 'GitHub' : 'Jira'}` : ''}
        </Button>
        {prefill && (
          <Card withBorder radius="sm" padding="xs" data-testid="defect-prefill">
            <Stack gap={6}>
              <Text size="xs">The tracker's create page cannot be filled in by a link (set the Jira project and issue type ids in the project settings for that). Copy the text and paste it into the new issue.</Text>
              <Textarea aria-label="Issue text" value={prefill.text} readOnly autosize minRows={3} maxRows={8} classNames={{ input: 'fp-mono' }} />
              <Group gap="xs">
                <CopyButton value={prefill.text}>
                  {({ copied, copy }) => (
                    <Button size="xs" variant="light" leftSection={copied ? <IconCheck size={14} /> : <IconCopy size={14} />} onClick={copy} data-testid="defect-copy-text">
                      {copied ? 'Copied' : 'Copy issue text'}
                    </Button>
                  )}
                </CopyButton>
                {prefill.url && (
                  <Anchor href={prefill.url} target="_blank" rel="noopener noreferrer" size="sm" data-testid="defect-create-link">
                    Open the create page
                  </Anchor>
                )}
              </Group>
            </Stack>
          </Card>
        )}
        {item.iterationCount > 0 && (
          <Text size="xs" c="dimmed" data-testid="defect-scope">
            {iteration ? `The defect is set on iteration #${String(iteration.position)} (${valuesLabel(item.parameterColumns, iteration.values)}) only.` : 'The defect is set on every failed or blocked iteration of this case.'}
          </Text>
        )}
        {!final && (
          <Text size="xs" c="dimmed" data-testid="defect-finish-note">
            A defect belongs to the case's result, so saving finishes the case as Failed first.
          </Text>
        )}
        <Text size="xs" c="dimmed">
          {tracker ? '' : 'No issue tracker is configured (Project settings → Integrations). '}The new issue opens in the tracker with the step, the expected and the actual result and a link back here (FastProof creates nothing in the tracker); paste its key above.
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
