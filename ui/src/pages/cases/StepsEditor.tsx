import { ActionIcon, Badge, Button, Card, FileButton, Group, Stack, Text, Tooltip } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconArrowDown, IconArrowUp, IconIndentDecrease, IconIndentIncrease, IconPaperclip, IconPlus, IconTrash, IconX } from '@tabler/icons-react';
import { useEffect, useRef, useState, type KeyboardEvent } from 'react';
import { errorMessage } from '../../api/client';
import { uploadCaseAttachment, type CaseStep } from '../../api/tm';
import { flattenSteps, indentStep, insertAfter, MAX_STEP_DEPTH, moveStep, outdentStep, removeStep, updateStep } from '../../lib/tm/steps';
import { editorAction } from '../runner/keymap';
import { MarkdownField } from './MarkdownField';

export const MAX_STEPS = 500;

export interface StepsEditorProps {
  project: string;
  caseNumber: number | null;
  steps: CaseStep[];
  onChange: (steps: CaseStep[]) => void;
  onAnnounce: (msg: string) => void;
}

export function StepsEditor({ project, caseNumber, steps, onChange, onAnnounce }: StepsEditorProps) {
  const flat = flattenSteps(steps);
  const [focusPath, setFocusPath] = useState<string | null>(null);
  const released = useRef(false);
  const container = useRef<HTMLDivElement>(null);
  const [uploading, setUploading] = useState<string | null>(null);

  useEffect(() => {
    if (!focusPath) return;
    const el = container.current?.querySelector<HTMLTextAreaElement>(`[data-step-path="${focusPath}"] textarea`);
    el?.focus();
    setFocusPath(null);
  }, [focusPath, steps]);

  const apply = (r: { steps: CaseStep[]; path: string } | null, msg: string) => {
    if (!r) {
      onAnnounce(`${msg}: not possible here`);
      return;
    }
    onChange(r.steps);
    setFocusPath(r.path);
    onAnnounce(`${msg}: now step ${r.path}`);
  };

  const add = (after: string | null) => {
    if (flat.length >= MAX_STEPS) {
      notifications.show({ color: 'yellow', title: 'Step limit', message: `A case holds at most ${String(MAX_STEPS)} steps.` });
      return;
    }
    apply(insertAfter(steps, after), 'Step added');
  };

  const keyDown = (path: string) => (e: KeyboardEvent<HTMLTextAreaElement>) => {
    const a = editorAction(e, { inStep: true, released: released.current });
    if (!a || a.type === 'save') return;
    if (a.type === 'release') {
      released.current = true;
      onAnnounce('Tab moves focus now');
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    if (a.type === 'newStepBelow') add(path);
    else if (a.type === 'moveStep') apply(moveStep(steps, path, a.dir), a.dir < 0 ? 'Moved up' : 'Moved down');
    else if (a.type === 'indent') apply(indentStep(steps, path), 'Nested');
    else apply(outdentStep(steps, path), 'Moved out');
  };

  const attach = async (path: string, file: File | null) => {
    if (!file || caseNumber === null) return;
    setUploading(path);
    try {
      const a = await uploadCaseAttachment(project, caseNumber, file, file.name);
      const s = flat.find((f) => f.path === path)?.step;
      if (s) onChange(updateStep(steps, path, { attachments: [...s.attachments, a.id].slice(0, 20) }));
    } catch (err) {
      notifications.show({ color: 'red', title: 'Attachment not uploaded', message: errorMessage(err) });
    } finally {
      setUploading(null);
    }
  };

  return (
    <Stack gap="xs" ref={container} data-testid="steps-editor">
      <Text size="xs" c="dimmed">
        Ctrl+Enter adds a step below, Alt+↑/↓ moves it, Tab and Shift+Tab nest it (Esc first to let Tab leave the field), Ctrl+S saves.
      </Text>
      {flat.map((f) => (
        <Card key={f.step.id} withBorder radius="md" padding="xs" className="fp-tm-step-edit" style={{ marginLeft: f.depth * 20 }} data-step-path={f.path} data-testid="step-edit" onFocusCapture={() => (released.current = false)}>
          <Group justify="space-between" gap={4} wrap="nowrap" mb={4}>
            <Badge size="sm" variant="light" radius="sm" className="fp-mono" aria-label={`Step ${f.path}`}>
              {f.path}
            </Badge>
            <Group gap={2} wrap="nowrap">
              <Tooltip label="Move up (Alt+↑)">
                <ActionIcon size="sm" variant="subtle" aria-label={`Move step ${f.path} up`} onClick={() => apply(moveStep(steps, f.path, -1), 'Moved up')}>
                  <IconArrowUp size={14} />
                </ActionIcon>
              </Tooltip>
              <Tooltip label="Move down (Alt+↓)">
                <ActionIcon size="sm" variant="subtle" aria-label={`Move step ${f.path} down`} onClick={() => apply(moveStep(steps, f.path, 1), 'Moved down')}>
                  <IconArrowDown size={14} />
                </ActionIcon>
              </Tooltip>
              <Tooltip label="Nest under the previous step (Tab)">
                <ActionIcon size="sm" variant="subtle" aria-label={`Nest step ${f.path}`} disabled={f.depth + 1 >= MAX_STEP_DEPTH} onClick={() => apply(indentStep(steps, f.path), 'Nested')}>
                  <IconIndentIncrease size={14} />
                </ActionIcon>
              </Tooltip>
              <Tooltip label="Move out of the parent (Shift+Tab)">
                <ActionIcon size="sm" variant="subtle" aria-label={`Move step ${f.path} out`} disabled={f.depth === 0} onClick={() => apply(outdentStep(steps, f.path), 'Moved out')}>
                  <IconIndentDecrease size={14} />
                </ActionIcon>
              </Tooltip>
              <FileButton onChange={(file) => void attach(f.path, file)} disabled={caseNumber === null}>
                {(props) => (
                  <Tooltip label="Attach a file">
                    <ActionIcon size="sm" variant="subtle" aria-label={`Attach a file to step ${f.path}`} loading={uploading === f.path} {...props}>
                      <IconPaperclip size={14} />
                    </ActionIcon>
                  </Tooltip>
                )}
              </FileButton>
              <Tooltip label="Delete the step">
                <ActionIcon
                  size="sm"
                  variant="subtle"
                  color="red"
                  aria-label={`Delete step ${f.path}`}
                  onClick={() => {
                    onChange(removeStep(steps, f.path));
                    onAnnounce(`Step ${f.path} deleted`);
                  }}
                >
                  <IconTrash size={14} />
                </ActionIcon>
              </Tooltip>
            </Group>
          </Group>
          <div className="fp-tm-step-grid">
            <MarkdownField project={project} caseNumber={caseNumber} label="Action" value={f.step.action} onChange={(v) => onChange(updateStep(steps, f.path, { action: v }))} compact onKeyDown={keyDown(f.path)} testId="step-action" inputId={`step-${f.step.id}-action`} minRows={1} />
            <MarkdownField project={project} caseNumber={caseNumber} label="Expected result" value={f.step.expected} onChange={(v) => onChange(updateStep(steps, f.path, { expected: v }))} compact onKeyDown={keyDown(f.path)} testId="step-expected" inputId={`step-${f.step.id}-expected`} minRows={1} />
            <MarkdownField project={project} caseNumber={caseNumber} label="Test data" value={f.step.data} onChange={(v) => onChange(updateStep(steps, f.path, { data: v }))} compact onKeyDown={keyDown(f.path)} testId="step-data" inputId={`step-${f.step.id}-data`} minRows={1} />
          </div>
          {f.step.attachments.length > 0 && (
            <Group gap={4} mt={4} wrap="wrap">
              {f.step.attachments.map((id) => (
                <Badge key={id} size="sm" variant="outline" color="gray" radius="sm" leftSection={<IconPaperclip size={10} />} rightSection={
                  <ActionIcon size="xs" variant="transparent" aria-label="Remove the attachment" onClick={() => onChange(updateStep(steps, f.path, { attachments: f.step.attachments.filter((x) => x !== id) }))}>
                    <IconX size={10} />
                  </ActionIcon>
                }>
                  {id.slice(0, 8)}
                </Badge>
              ))}
            </Group>
          )}
        </Card>
      ))}
      <Group>
        <Button size="xs" variant="light" leftSection={<IconPlus size={14} />} onClick={() => add(flat.length ? (flat.filter((x) => x.depth === 0).pop()?.path ?? null) : null)} data-testid="step-add">
          Add step
        </Button>
        <Text size="xs" c="dimmed">
          {flat.length} / {MAX_STEPS} steps
        </Text>
      </Group>
    </Stack>
  );
}
