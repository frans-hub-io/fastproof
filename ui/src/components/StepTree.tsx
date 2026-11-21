import { Badge, Collapse, Group, Stack, Text, Tooltip, UnstyledButton } from '@mantine/core';
import { IconChevronDown, IconChevronRight, IconPaperclip } from '@tabler/icons-react';
import { useState } from 'react';
import type { Step } from '../api/types';
import { formatDuration } from '../lib/format';
import { statusVar } from '../lib/status';
import { AttachmentList } from '../viewers/AttachmentList';
import { ErrorBlock } from './ErrorBlock';
import { StatusDot } from './Status';

export interface Timeline {
  start: number;
  total: number;
}

function Timing({ step, timeline }: { step: Step; timeline: Timeline | null }) {
  if (!timeline || !step.startedAt || timeline.total <= 0) return null;
  const s = new Date(step.startedAt).getTime() - timeline.start;
  const d = step.durationMs ?? 0;
  const left = Math.max(0, Math.min(100, (s / timeline.total) * 100));
  const width = Math.max(0.8, Math.min(100 - left, (d / timeline.total) * 100));
  return (
    <div className="fp-step-timing" style={{ width: 120, flex: 'none' }} aria-hidden>
      <span style={{ left: `${String(left)}%`, width: `${String(width)}%`, background: statusVar(step.status) }} />
    </div>
  );
}

function StepNode({ project, step, timeline, depth }: { project: string; step: Step; timeline: Timeline | null; depth: number }) {
  const failing = step.status === 'failed' || step.status === 'broken';
  const hasBody = step.steps.length > 0 || step.attachments.length > 0 || step.parameters.length > 0 || !!step.error;
  const [open, setOpen] = useState(failing);
  return (
    <div data-testid="step" data-status={step.status}>
      <UnstyledButton onClick={() => hasBody && setOpen((o) => !o)} className="fp-tree-row" style={{ display: 'block', width: '100%' }} aria-expanded={hasBody ? open : undefined}>
        <Group gap={6} wrap="nowrap">
          <span style={{ width: 14, display: 'inline-flex', visibility: hasBody ? 'visible' : 'hidden' }}>{open ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}</span>
          <StatusDot status={step.status} />
          <Text size="sm" style={{ flex: 1, minWidth: 0, wordBreak: 'break-word' }} fw={failing ? 600 : 400}>
            {step.name}
          </Text>
          {step.kind === 'fixture' && (
            <Badge size="xs" variant="outline" color="gray" radius="sm" style={{ textTransform: 'none', flex: 'none' }}>
              fixture
            </Badge>
          )}
          {step.attachments.length > 0 && (
            <Tooltip label={`${String(step.attachments.length)} attachments`}>
              <Badge size="xs" variant="light" color="gray" radius="sm" style={{ flex: 'none' }}>
                <IconPaperclip size={10} /> {step.attachments.length}
              </Badge>
            </Tooltip>
          )}
          <Group gap={8} wrap="nowrap" visibleFrom="sm" style={{ flex: 'none' }}>
            <Timing step={step} timeline={timeline} />
          </Group>
          <Text size="xs" c="dimmed" style={{ width: 64, textAlign: 'right', flex: 'none' }}>
            {formatDuration(step.durationMs)}
          </Text>
        </Group>
      </UnstyledButton>
      {hasBody && (
        <Collapse expanded={open}>
          <Stack gap={6} className="fp-step" py={4}>
            {step.parameters.length > 0 && (
              <Group gap={6} wrap="wrap">
                {step.parameters.map((p) => (
                  <Badge key={p.name} variant="light" color="fhPurple" radius="sm" style={{ textTransform: 'none' }}>
                    {p.name}: {p.value}
                  </Badge>
                ))}
              </Group>
            )}
            {step.error && <ErrorBlock error={step.error} compact />}
            {step.attachments.length > 0 && <AttachmentList project={project} attachments={step.attachments} compact />}
            {step.steps.map((s, i) => (
              <StepNode key={`${s.name}-${String(i)}`} project={project} step={s} timeline={timeline} depth={depth + 1} />
            ))}
          </Stack>
        </Collapse>
      )}
    </div>
  );
}

export function StepTree({ project, steps, timeline }: { project: string; steps: Step[]; timeline: Timeline | null }) {
  if (!steps.length) {
    return (
      <Text size="sm" c="dimmed">
        No steps were reported.
      </Text>
    );
  }
  return (
    <Stack gap={0} data-testid="step-tree">
      {steps.map((s, i) => (
        <StepNode key={`${s.name}-${String(i)}`} project={project} step={s} timeline={timeline} depth={0} />
      ))}
    </Stack>
  );
}
