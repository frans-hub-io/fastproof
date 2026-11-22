import { Badge, Button, Group, Modal, ScrollArea, Stack, Text, TextInput, Tooltip, UnstyledButton } from '@mantine/core';
import {
  IconBan,
  IconCircleCheck,
  IconCircleDashed,
  IconCircleX,
  IconClockExclamation,
  IconCode,
  IconFolder,
  IconHandFinger,
  IconPencil,
  IconPlayerSkipForward,
  IconProgress,
  IconRobot,
  IconSearch,
  IconSparkles,
} from '@tabler/icons-react';
import { useMemo, useState, type ReactNode } from 'react';
import type { CaseFolder, CaseStatus, RunStatus, StepStatus } from '../../api/tm';
import { folderPathLabel, runStatusVar } from '../../lib/tm/ui';
import { CASE_STATUS_LABEL } from '../../lib/tm/filters';
import { RUN_STATUS_LABEL } from '../../lib/tm/steps';

const CASE_STATUS_ICON: Record<CaseStatus, typeof IconPencil> = { draft: IconPencil, active: IconCircleCheck, outdated: IconClockExclamation };
const CASE_STATUS_COLOR: Record<CaseStatus, string> = { draft: 'var(--fp-tm-case-draft)', active: 'var(--fp-tm-case-active)', outdated: 'var(--fp-tm-case-outdated)' };

export function CaseStatusBadge({ status, size = 'sm' }: { status: CaseStatus; size?: 'xs' | 'sm' | 'md' }) {
  const Icon = CASE_STATUS_ICON[status];
  return (
    <Badge
      size={size}
      variant="light"
      radius="sm"
      leftSection={<Icon size={11} aria-hidden />}
      data-case-status={status}
      styles={{ root: { flex: 'none', color: CASE_STATUS_COLOR[status], background: `color-mix(in srgb, ${CASE_STATUS_COLOR[status]} 14%, transparent)` } }}
    >
      {CASE_STATUS_LABEL[status]}
    </Badge>
  );
}

const RUN_ICON: Record<RunStatus, typeof IconPencil> = {
  untested: IconCircleDashed,
  in_progress: IconProgress,
  passed: IconCircleCheck,
  failed: IconCircleX,
  blocked: IconBan,
  skipped: IconPlayerSkipForward,
};


export function RunStatusIcon({ status, size = 16 }: { status: RunStatus | StepStatus; size?: number }) {
  const Icon = RUN_ICON[status];
  return <Icon size={size} color={runStatusVar(status)} aria-hidden style={{ flex: 'none' }} />;
}

/** A run or step status as icon and text (never colour alone). */
export function RunStatusBadge({ status, size = 'sm' }: { status: RunStatus | StepStatus; size?: 'xs' | 'sm' | 'md' }) {
  return (
    <Badge
      size={size}
      variant="light"
      radius="sm"
      leftSection={<RunStatusIcon status={status} size={12} />}
      styles={{ root: { color: runStatusVar(status), background: `color-mix(in srgb, ${runStatusVar(status)} 14%, transparent)`, flex: 'none' } }}
      data-run-status={status}
    >
      {RUN_STATUS_LABEL[status]}
    </Badge>
  );
}

export function AutomationIcon({ automated, size = 16 }: { automated: boolean; size?: number }) {
  const label = automated ? 'Automated' : 'Manual';
  const Icon = automated ? IconRobot : IconHandFinger;
  return (
    <Tooltip label={label}>
      <span role="img" aria-label={label} style={{ display: 'inline-flex', flex: 'none', color: 'var(--mantine-color-dimmed)' }} data-automated={automated}>
        <Icon size={size} aria-hidden />
      </span>
    </Tooltip>
  );
}

export function ManualIcon({ size = 14 }: { size?: number }) {
  return (
    <Tooltip label="Manual result">
      <span role="img" aria-label="Manual result" style={{ display: 'inline-flex', flex: 'none', color: 'var(--mantine-color-dimmed)' }} data-testid="manual-icon">
        <IconHandFinger size={size} aria-hidden />
      </span>
    </Tooltip>
  );
}

export function AiDraftBadge() {
  return (
    <Tooltip label="Created by an AI agent and not reviewed yet">
      <Badge
        size="sm"
        variant="light"
        radius="sm"
        leftSection={<IconSparkles size={11} aria-hidden />}
        data-testid="ai-draft-badge"
        styles={{ root: { flex: 'none', color: 'var(--fp-tm-ai)', background: 'color-mix(in srgb, var(--fp-tm-ai) 14%, transparent)' } }}
      >
        AI draft
      </Badge>
    </Tooltip>
  );
}

export function FromCodeBadge({ hint }: { hint?: string }) {
  return (
    <Tooltip label={hint ?? 'Set by the code sync from trusted launches; change it in the test code'} multiline maw={280}>
      <Badge size="xs" variant="outline" color="gray" radius="sm" leftSection={<IconCode size={10} aria-hidden />} data-testid="from-code" style={{ flex: 'none' }} tabIndex={0}>
        from code
      </Badge>
    </Tooltip>
  );
}



/** The keyboard alternative to dragging cases onto a folder: a searchable list of folders. */
export function FolderPicker({ opened, onClose, folders, onPick, title = 'Move to folder', allowRoot = true, busy = false }: { opened: boolean; onClose: () => void; folders: readonly CaseFolder[]; onPick: (id: string | null) => void; title?: string; allowRoot?: boolean; busy?: boolean }) {
  const [q, setQ] = useState('');
  const [active, setActive] = useState(0);
  const options = useMemo(() => {
    const list: { id: string | null; label: string }[] = folders.map((f) => ({ id: f.id, label: folderPathLabel(folders, f.id) })).sort((a, b) => a.label.localeCompare(b.label));
    const all = allowRoot ? [{ id: null, label: 'No folder (project root)' }, ...list] : list;
    const t = q.trim().toLowerCase();
    return t ? all.filter((o) => o.label.toLowerCase().includes(t)) : all;
  }, [folders, q, allowRoot]);
  const pick = (i: number) => {
    const o = options[i];
    if (o) onPick(o.id);
  };
  return (
    <Modal opened={opened} onClose={onClose} title={title} size="md">
      <Stack gap="xs">
        <TextInput
          data-autofocus
          aria-label="Find a folder"
          placeholder="Type to find a folder"
          leftSection={<IconSearch size={14} />}
          value={q}
          onChange={(e) => {
            setQ(e.currentTarget.value);
            setActive(0);
          }}
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown') {
              e.preventDefault();
              setActive((a) => Math.min(options.length - 1, a + 1));
            } else if (e.key === 'ArrowUp') {
              e.preventDefault();
              setActive((a) => Math.max(0, a - 1));
            } else if (e.key === 'Enter') {
              e.preventDefault();
              pick(active);
            }
          }}
          aria-activedescendant={options[active] ? `folder-opt-${String(active)}` : undefined}
          aria-controls="folder-picker-list"
        />
        <ScrollArea.Autosize mah={320}>
          <div role="listbox" id="folder-picker-list" aria-label="Folders">
            {options.map((o, i) => (
              <UnstyledButton
                key={o.id ?? 'root'}
                id={`folder-opt-${String(i)}`}
                role="option"
                aria-selected={i === active}
                className="fp-row"
                data-selected={i === active}
                onClick={() => pick(i)}
                onMouseEnter={() => setActive(i)}
                style={{ display: 'flex', gap: 8, alignItems: 'center', width: '100%', padding: '6px 8px' }}
                data-testid="folder-option"
                disabled={busy}
              >
                <IconFolder size={14} aria-hidden />
                <Text size="sm">{o.label}</Text>
              </UnstyledButton>
            ))}
            {!options.length && (
              <Text size="sm" c="dimmed" p="sm">
                No folder matches.
              </Text>
            )}
          </div>
        </ScrollArea.Autosize>
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}

export function Section({ title, children, actions }: { title: string; children: ReactNode; actions?: ReactNode }) {
  return (
    <Stack gap={6}>
      <Group justify="space-between" gap="xs">
        <Text fw={600} size="sm" component="h3" m={0}>
          {title}
        </Text>
        {actions}
      </Group>
      {children}
    </Stack>
  );
}

/** Polite live region for announcements ("Step 3: passed"). */
export function LiveRegion({ message }: { message: string }) {
  return (
    <div aria-live="polite" aria-atomic="true" className="fp-sr-only" data-testid="live-region">
      {message}
    </div>
  );
}
