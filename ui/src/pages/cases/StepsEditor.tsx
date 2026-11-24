import { ActionIcon, Badge, Button, Card, FileButton, Group, Stack, Text, Tooltip } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconArrowDown, IconArrowUp, IconChevronDown, IconChevronRight, IconIndentDecrease, IconIndentIncrease, IconLink, IconPaperclip, IconPlus, IconTrash, IconX } from '@tabler/icons-react';
import { useEffect, useRef, useState, type KeyboardEvent } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { sharedKey, type SharedStepBrief } from '../../api/sharedSteps';
import { tmk, uploadCaseAttachment, type Case, type CaseStep, type Versioned } from '../../api/tm';
import { flattenSteps, indentStep, insertAfter, MAX_STEP_DEPTH, moveStep, newStepId, outdentStep, removeStep, updateStep } from '../../lib/tm/steps';
import { editorAction } from '../runner/keymap';
import { SharedPicker } from '../shared/SharedPicker';
import { MarkdownField } from './MarkdownField';

export const MAX_STEPS = 500;

export interface StepsEditorProps {
  project: string;
  caseNumber: number | null;
  steps: CaseStep[];
  onChange: (steps: CaseStep[]) => void;
  onAnnounce: (msg: string) => void;
  /** The shared steps the case references (`Case.sharedSteps`), to show their names. */
  briefs?: SharedStepBrief[];
  /** Offer "Insert shared step" (Ctrl+Shift+S); a shared step itself cannot hold shared steps. Default true. */
  allowShared?: boolean;
  /** Deepest nesting of steps (3 for a case, 2 for a shared step). */
  maxDepth?: number;
  /** Offer file attachments on steps (a shared step has none). Default true. */
  attachments?: boolean;
}

function isInsertShortcut(e: { key: string; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean; altKey: boolean }): boolean {
  return (e.ctrlKey || e.metaKey) && e.shiftKey && !e.altKey && e.key.toLowerCase() === 's';
}

export function StepsEditor({ project, caseNumber, steps, onChange, onAnnounce, briefs = [], allowShared = true, maxDepth = MAX_STEP_DEPTH, attachments = true }: StepsEditorProps) {
  const flat = flattenSteps(steps);
  const [picking, setPicking] = useState(false);
  const [picked, setPicked] = useState<Record<string, SharedStepBrief>>({});
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  const lastPath = useRef<string | null>(null);
  const qc = useQueryClient();
  const saved = caseNumber === null ? undefined : qc.getQueryData<Versioned<Case>>(tmk.case(project, caseNumber))?.value.sharedSteps;
  const briefOf = (id: string): SharedStepBrief | undefined => picked[id] ?? briefs.find((b) => b.id === id) ?? saved?.find((b) => b.id === id);
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

  const insertShared = (b: SharedStepBrief) => {
    if (flat.length >= MAX_STEPS) {
      notifications.show({ color: 'yellow', title: 'Step limit', message: `A case holds at most ${String(MAX_STEPS)} steps.` });
      return;
    }
    setPicked((p) => ({ ...p, [b.id]: b }));
    const top = lastPath.current?.split('.')[0] ?? null;
    const ref: CaseStep = { id: newStepId(), action: '', expected: '', data: '', attachments: [], steps: [], shared: { id: b.id, number: b.number } };
    const r = insertAfter(steps, top, ref);
    onChange(r.steps);
    onAnnounce(`${sharedKey(b.number)} inserted as step ${r.path}`);
  };

  const keyDown = (path: string) => (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (allowShared && isInsertShortcut(e)) {
      e.preventDefault();
      e.stopPropagation();
      lastPath.current = path;
      setPicking(true);
      return;
    }
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
    else if (a.type === 'indent') apply(indentStep(steps, path, maxDepth), 'Nested');
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
    <Stack
      gap="xs"
      ref={container}
      data-testid="steps-editor"
      onKeyDown={(e) => {
        if (allowShared && isInsertShortcut(e)) {
          e.preventDefault();
          e.stopPropagation();
          setPicking(true);
        }
      }}
    >
      <Text size="xs" c="dimmed">
        Ctrl+Enter adds a step below, Alt+↑/↓ moves it, Tab and Shift+Tab nest it (Esc first to let Tab leave the field), Ctrl+S saves
        {allowShared ? ', Ctrl+Shift+S inserts a shared step' : ''}.
      </Text>
      {flat.map((f) => {
        const ref = f.step.shared;
        if (ref) {
          const b = briefOf(ref.id);
          const isOpen = open.has(f.step.id);
          return (
            <Card key={f.step.id} withBorder radius="md" padding="xs" className="fp-tm-step-edit fp-tm-shared-step" data-step-path={f.path} data-testid="step-shared" onFocusCapture={() => (lastPath.current = f.path)}>
              <Group justify="space-between" gap={4} wrap="nowrap">
                <Group gap={6} wrap="nowrap" style={{ minWidth: 0 }}>
                  <Badge size="sm" variant="light" radius="sm" className="fp-mono" aria-label={`Step ${f.path}`}>
                    {f.path}
                  </Badge>
                  <ActionIcon size="sm" variant="subtle" aria-expanded={isOpen} aria-label={`${isOpen ? 'Hide' : 'Show'} the steps of ${sharedKey(ref.number)}`} onClick={() => setOpen((o) => new Set(isOpen ? [...o].filter((x) => x !== f.step.id) : [...o, f.step.id]))}>
                    {isOpen ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
                  </ActionIcon>
                  <Badge size="sm" variant="outline" radius="sm" className="fp-mono" leftSection={<IconLink size={10} />}>
                    {sharedKey(ref.number)}
                  </Badge>
                  <Text size="sm" fw={500} className="fh-truncate" component={Link} {...{ to: `/p/${project}/shared-steps/${String(ref.number)}` }}>
                    {b?.name ?? 'Shared step'}
                  </Text>
                  {b && (
                    <Text size="xs" c="dimmed">
                      v{b.version}
                      {b.archived ? ' · archived' : ''}
                    </Text>
                  )}
                </Group>
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
                  <Tooltip label="Remove the reference">
                    <ActionIcon
                      size="sm"
                      variant="subtle"
                      color="red"
                      aria-label={`Delete step ${f.path}`}
                      onClick={() => {
                        onChange(removeStep(steps, f.path));
                        onAnnounce(`${sharedKey(ref.number)} removed`);
                      }}
                    >
                      <IconTrash size={14} />
                    </ActionIcon>
                  </Tooltip>
                </Group>
              </Group>
              {isOpen && (
                <Stack gap={2} mt={6} pl={30} data-testid="step-shared-preview">
                  {b ? (
                    flattenSteps(b.steps).map((x) => (
                      <Text key={`${x.path}-${x.step.id}`} size="xs" c="dimmed" style={{ marginLeft: x.depth * 14 }} className="fh-truncate">
                        {x.path}. {x.step.action || '—'}
                      </Text>
                    ))
                  ) : (
                    <Text size="xs" c="dimmed">
                      Save the case to see the steps.
                    </Text>
                  )}
                </Stack>
              )}
            </Card>
          );
        }
        return (
        <Card key={f.step.id} withBorder radius="md" padding="xs" className="fp-tm-step-edit" style={{ marginLeft: f.depth * 20 }} data-step-path={f.path} data-testid="step-edit" onFocusCapture={() => ((released.current = false), (lastPath.current = f.path))}>
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
                <ActionIcon size="sm" variant="subtle" aria-label={`Nest step ${f.path}`} disabled={f.depth + 1 >= maxDepth} onClick={() => apply(indentStep(steps, f.path, maxDepth), 'Nested')}>
                  <IconIndentIncrease size={14} />
                </ActionIcon>
              </Tooltip>
              <Tooltip label="Move out of the parent (Shift+Tab)">
                <ActionIcon size="sm" variant="subtle" aria-label={`Move step ${f.path} out`} disabled={f.depth === 0} onClick={() => apply(outdentStep(steps, f.path), 'Moved out')}>
                  <IconIndentDecrease size={14} />
                </ActionIcon>
              </Tooltip>
              {attachments && (
                <FileButton onChange={(file) => void attach(f.path, file)} disabled={caseNumber === null}>
                  {(props) => (
                    <Tooltip label="Attach a file">
                      <ActionIcon size="sm" variant="subtle" aria-label={`Attach a file to step ${f.path}`} loading={uploading === f.path} {...props}>
                        <IconPaperclip size={14} />
                      </ActionIcon>
                    </Tooltip>
                  )}
                </FileButton>
              )}
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
        );
      })}
      <Group>
        <Button size="xs" variant="light" leftSection={<IconPlus size={14} />} onClick={() => add(flat.length ? (flat.filter((x) => x.depth === 0).pop()?.path ?? null) : null)} data-testid="step-add">
          Add step
        </Button>
        {allowShared && (
          <Button
            size="xs"
            variant="light"
            leftSection={<IconLink size={14} />}
            onClick={() => {
              lastPath.current = lastPath.current ?? flat.filter((x) => x.depth === 0).pop()?.path ?? null;
              setPicking(true);
            }}
            data-testid="step-insert-shared"
          >
            Insert shared step
          </Button>
        )}
        <Text size="xs" c="dimmed">
          {flat.length} / {MAX_STEPS} steps
        </Text>
      </Group>
      {allowShared && <SharedPicker project={project} opened={picking} onClose={() => setPicking(false)} onPick={insertShared} />}
    </Stack>
  );
}
