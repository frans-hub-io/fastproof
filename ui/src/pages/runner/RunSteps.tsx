import { ActionIcon, Badge, Button, Group, Stack, Text, Textarea, Tooltip } from '@mantine/core';
import { IconBan, IconCamera, IconChevronDown, IconChevronRight, IconCircleCheck, IconCircleDashed, IconCircleX, IconLink, IconPaperclip, IconPlayerSkipForward } from '@tabler/icons-react';
import { useEffect, useState, type ReactNode, type RefObject } from 'react';
import { attachmentUrl } from '../../api/client';
import type { CaseStep, RunItem, StepStatus } from '../../api/tm';
import type { FlatStep } from '../../lib/tm/steps';
import { RUN_STATUS_LABEL } from '../../lib/tm/steps';
import { CaseHtml } from '../cases/CaseHtml';
import { RunStatusBadge } from '../cases/common';
import { htmlFor, type LocalStep } from './types';

const SHORT: Record<StepStatus, string> = { passed: 'Pass', failed: 'Fail', blocked: 'Block', skipped: 'Skip', untested: 'Reset' };

const STATUS_BUTTONS: { status: StepStatus; label: string; key: string; color: string; Icon: typeof IconBan }[] = [
  { status: 'passed', label: 'Pass', key: 'p', color: 'green', Icon: IconCircleCheck },
  { status: 'failed', label: 'Fail', key: 'f', color: 'red', Icon: IconCircleX },
  { status: 'blocked', label: 'Blocked', key: 'b', color: 'orange', Icon: IconBan },
  { status: 'skipped', label: 'Skip', key: 's', color: 'gray', Icon: IconPlayerSkipForward },
];


export interface StepFieldsProps {
  project: string;
  path: string;
  local: LocalStep;
  readOnly: boolean;
  onText: (field: 'actual' | 'comment', value: string) => void;
  onFiles: (files: FileList | null) => void;
  actualRef: RefObject<HTMLTextAreaElement | null>;
  commentRef: RefObject<HTMLTextAreaElement | null>;
  fileRef: RefObject<HTMLInputElement | null>;
  uploading: boolean;
}

/** Actual result, comment and attachments of the current step. */
export function StepFields({ project, path, local, readOnly, onText, onFiles, actualRef, commentRef, fileRef, uploading }: StepFieldsProps) {
  return (
    <Stack gap={6} className="fp-tm-step-fields">
      <Textarea ref={actualRef} label="Actual result" value={local.actual} onChange={(e) => onText('actual', e.currentTarget.value)} autosize minRows={1} maxRows={8} maxLength={16384} readOnly={readOnly} data-testid="step-actual" description="Esc leaves the field" />
      <Textarea ref={commentRef} label="Comment" value={local.comment} onChange={(e) => onText('comment', e.currentTarget.value)} autosize minRows={1} maxRows={6} maxLength={16384} readOnly={readOnly} data-testid="step-comment" />
      <Group gap={6} wrap="wrap">
        {local.attachments.map((a) => (
          <Badge key={a.id} component="a" href={attachmentUrl(project, a.id)} target="_blank" rel="noopener noreferrer" variant="outline" color="gray" radius="sm" leftSection={<IconPaperclip size={10} />} style={{ cursor: 'pointer', textTransform: 'none' }} data-testid="step-attachment">
            {a.name}
          </Badge>
        ))}
        {!readOnly && (
          <>
            <input ref={fileRef} type="file" multiple hidden onChange={(e) => onFiles(e.currentTarget.files)} data-testid="step-file" aria-label={`Attach files to step ${path}`} />
            <Button size="compact-xs" variant="subtle" leftSection={<IconPaperclip size={12} />} onClick={() => fileRef.current?.click()} loading={uploading}>
              Attach (a)
            </Button>
            <Button size="compact-xs" variant="subtle" leftSection={<IconCamera size={12} />} component="label" className="fp-tm-camera">
              Camera
              <input type="file" accept="image/*,video/*" capture="environment" hidden onChange={(e) => onFiles(e.currentTarget.files)} data-testid="step-camera" />
            </Button>
          </>
        )}
      </Group>
    </Stack>
  );
}

export function StatusButtons({ path, current, readOnly, onSet, size = 'xs' }: { path: string; current: StepStatus; readOnly: boolean; onSet: (s: StepStatus) => void; size?: 'xs' | 'lg' }) {
  return (
    <Group gap={4} wrap="nowrap" role="group" aria-label={`Status of step ${path}`} className={size === 'lg' ? 'fp-tm-bigbar' : undefined}>
      {STATUS_BUTTONS.map(({ status, label, key, color, Icon }) => (
        <Tooltip key={status} label={`${label} (${key})`} disabled={size === 'lg'}>
          <Button
            size={size === 'lg' ? 'md' : 'compact-xs'}
            variant={current === status ? 'filled' : 'light'}
            color={color}
            onClick={() => onSet(status)}
            disabled={readOnly}
            aria-pressed={current === status}
            leftSection={<Icon size={size === 'lg' ? 18 : 12} aria-hidden />}
            data-testid={`set-${status}`}
            style={size === 'lg' ? { minHeight: 44, flex: 1 } : undefined}
          >
            {size === 'lg' ? SHORT[status] : label}
          </Button>
        </Tooltip>
      ))}
      {size !== 'lg' && (
        <Tooltip label="Reset (u)">
          <Button size="compact-xs" variant="subtle" color="gray" onClick={() => onSet('untested')} disabled={readOnly || current === 'untested'} aria-label={`Reset step ${path}`} leftSection={<IconCircleDashed size={12} aria-hidden />}>
            Reset
          </Button>
        </Tooltip>
      )}
    </Group>
  );
}

/** The title of a shared step in a run: `SS-12 v4`, its name and whether a newer version exists. */
export function SharedHead({ item, step, toggle }: { item: RunItem; step: CaseStep; toggle?: ReactNode }) {
  const ref = step.shared;
  if (!ref) return null;
  const pin = item.sharedPins.find((p) => p.id === ref.id);
  return (
    <Group gap={6} wrap="wrap" data-testid="run-shared-head">
      {toggle}
      <Badge size="sm" variant="outline" radius="sm" className="fp-mono" leftSection={<IconLink size={10} />}>
        SS-{ref.number} v{ref.version ?? pin?.version ?? '?'}
      </Badge>
      <Text size="sm" fw={500}>
        {pin?.name ?? 'Shared step'}
      </Text>
      {pin && pin.current !== pin.version && (
        <Badge size="xs" variant="light" color="yellow" radius="sm">
          v{pin.current} available
        </Badge>
      )}
    </Group>
  );
}

export interface RunStepsProps {
  project: string;
  item: RunItem;
  flat: FlatStep[];
  effective: ReadonlyMap<string, StepStatus>;
  local: ReadonlyMap<string, LocalStep>;
  current: number;
  onCurrent: (i: number) => void;
  onSet: (path: string, s: StepStatus) => void;
  fields: Omit<StepFieldsProps, 'path' | 'local' | 'project'>;
}

/** Desktop: every step in a list, the current one expanded with its result fields. */
export function RunSteps({ project, item, flat, effective, local, current, onCurrent, onSet, fields }: RunStepsProps) {
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  const hiddenBy = (path: string): string | undefined => [...collapsed].find((c) => path.startsWith(`${c}.`));
  const currentPath = flat[current]?.path;
  useEffect(() => {
    if (!currentPath) return;
    const inside = [...collapsed].find((c) => currentPath.startsWith(`${c}.`));
    if (inside) setCollapsed((c) => new Set([...c].filter((x) => x !== inside)));
  }, [currentPath, collapsed]);
  return (
    <ol className="fp-tm-run-steps" aria-label="Steps" data-testid="run-steps">
      {flat.map((f, i) => {
        const html = htmlFor(item, f.path);
        const ref = f.step.shared;
        const isCollapsed = collapsed.has(f.path);
        const own = local.get(f.path);
        const status = effective.get(f.path) ?? 'untested';
        const derived = f.hasChildren && (!own || own.status === 'untested') && status !== 'untested';
        const active = i === current;
        return (
          <li key={`${f.path}-${f.step.id}`} className="fp-tm-run-step" data-current={active} data-status={status} data-shared={ref ? 'true' : undefined} data-testid="run-step" data-path={f.path} style={{ marginLeft: f.depth * 18 }} aria-current={active ? 'step' : undefined} tabIndex={active ? -1 : undefined} hidden={hiddenBy(f.path) !== undefined}>
            <div className="fp-tm-run-step-head" onClick={() => onCurrent(i)}>
              <Text size="xs" c="dimmed" className="fp-mono" style={{ width: 36, flex: 'none' }}>
                {f.path}
              </Text>
              <div style={{ flex: 1, minWidth: 0 }}>
                {ref ? (
                  <SharedHead
                    item={item}
                    step={f.step}
                    toggle={
                      <ActionIcon
                        size="sm"
                        variant="subtle"
                        aria-expanded={!isCollapsed}
                        aria-label={`${isCollapsed ? 'Show' : 'Hide'} the steps of SS-${String(ref.number)}`}
                        onClick={(e) => {
                          e.stopPropagation();
                          setCollapsed((c) => new Set(isCollapsed ? [...c].filter((x) => x !== f.path) : [...c, f.path]));
                        }}
                        data-testid="run-shared-toggle"
                      >
                        {isCollapsed ? <IconChevronRight size={14} /> : <IconChevronDown size={14} />}
                      </ActionIcon>
                    }
                  />
                ) : (
                  <CaseHtml html={html?.action ?? ''} project={project} />
                )}
                {f.step.expected && (
                  <div className="fp-tm-expected">
                    <Text span size="xs" c="dimmed">
                      Expected:{' '}
                    </Text>
                    <CaseHtml html={html?.expected ?? ''} project={project} />
                  </div>
                )}
                {f.step.data && (
                  <div className="fp-tm-expected">
                    <Text span size="xs" c="dimmed">
                      Data:{' '}
                    </Text>
                    <CaseHtml html={html?.data ?? ''} project={project} />
                  </div>
                )}
              </div>
              <Stack gap={2} align="flex-end" style={{ flex: 'none' }}>
                <RunStatusBadge status={status} size="xs" />
                {derived && (
                  <Text size="xs" c="dimmed">
                    from sub-steps
                  </Text>
                )}
              </Stack>
            </div>
            {active && (
              <Stack gap={6} mt={6}>
                <StatusButtons path={f.path} current={own?.status ?? 'untested'} readOnly={fields.readOnly} onSet={(s) => onSet(f.path, s)} />
                <StepFields {...fields} project={project} path={f.path} local={own ?? { status: 'untested', actual: '', comment: '', attachments: [] }} />
              </Stack>
            )}
            {!active && own && (own.actual || own.comment || own.attachments.length > 0) && (
              <Text size="xs" c="dimmed" mt={2} className="fh-truncate">
                {own.actual ? `Actual: ${own.actual}` : own.comment ? `Comment: ${own.comment}` : ''}
                {own.attachments.length ? ` · ${String(own.attachments.length)} attachments` : ''}
              </Text>
            )}
          </li>
        );
      })}
      {!flat.length && (
        <Text size="sm" c="dimmed">
          This case has no steps; choose its status below. {RUN_STATUS_LABEL.untested} until then.
        </Text>
      )}
    </ol>
  );
}

