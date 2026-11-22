import { ActionIcon, Anchor, Autocomplete, Badge, Button, Card, Group, Stack, Text, TextInput, Tooltip } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconFolder, IconSearch, IconUserCheck, IconX } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { useChangeTestPlanItems, type CaseFolder, type TestPlanItem } from '../../api/tm';
import { caseLabel } from '../../lib/tm/filters';
import { caseLink, folderPathLabel } from '../../lib/tm/ui';
import { AutomationIcon } from './common';

const SHOWN = 100;

function AssigneeInput({ value, people, onCommit, label, testId }: { value: string | null; people: string[]; onCommit: (v: string | null) => void; label: string; testId?: string }) {
  const [text, setText] = useState(value ?? '');
  const commit = (v: string) => {
    const next = v.trim() || null;
    if (next !== value) onCommit(next);
  };
  return (
    <Autocomplete
      size="xs"
      aria-label={label}
      placeholder="Unassigned"
      data={people}
      value={text}
      onChange={setText}
      onOptionSubmit={(v) => {
        setText(v);
        commit(v);
      }}
      onBlur={() => commit(text)}
      onKeyDown={(e) => {
        if (e.key === 'Enter') commit(text);
      }}
      w={210}
      comboboxProps={{ withinPortal: true }}
      data-testid={testId}
    />
  );
}

function FolderAssign({ people, onApply, folder }: { people: string[]; onApply: (v: string | null) => void; folder: string }) {
  const [text, setText] = useState('');
  return (
    <Group gap={4} wrap="nowrap">
      <Autocomplete size="xs" aria-label={`Assign every case in ${folder} to`} placeholder="Assign all to…" data={people} value={text} onChange={setText} w={180} comboboxProps={{ withinPortal: true }} data-testid="folder-assign-input" />
      <Tooltip label="Assign the whole folder">
        <ActionIcon variant="light" size="md" aria-label={`Assign every case in ${folder}`} onClick={() => onApply(text.trim() || null)} data-testid="folder-assign-apply">
          <IconUserCheck size={14} />
        </ActionIcon>
      </Tooltip>
    </Group>
  );
}

export function PlanItems({ project, planId, items, folders, people, readOnly }: { project: string; planId: string; items: TestPlanItem[]; folders: CaseFolder[]; people: string[]; readOnly: boolean }) {
  const change = useChangeTestPlanItems(project, planId);
  const [q, setQ] = useState('');
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const groups = useMemo(() => {
    const t = q.trim().toLowerCase();
    const m = new Map<string, TestPlanItem[]>();
    for (const it of items) {
      if (t && !`${caseLabel(it.caseNumber)} ${it.name} ${it.assignee ?? ''}`.toLowerCase().includes(t)) continue;
      const k = it.folderId ?? '';
      const list = m.get(k) ?? [];
      list.push(it);
      m.set(k, list);
    }
    return [...m.entries()].map(([id, list]) => ({ id, label: id ? folderPathLabel(folders, id) : 'No folder', list })).sort((a, b) => a.label.localeCompare(b.label));
  }, [items, folders, q]);

  const run = (body: Parameters<typeof change.mutate>[0], done: string) =>
    change.mutate(body, {
      onSuccess: () => notifications.show({ color: 'green', title: done, message: '' }),
      onError: (e) => notifications.show({ color: 'red', title: 'Plan not changed', message: errorMessage(e) }),
    });

  return (
    <Stack gap="xs" data-testid="plan-items">
      <Group justify="space-between" gap="xs" wrap="wrap">
        <Text size="sm" fw={600}>
          In the plan: {items.length}
        </Text>
        <TextInput size="xs" aria-label="Filter the plan" placeholder="Filter by case or assignee" leftSection={<IconSearch size={12} />} value={q} onChange={(e) => setQ(e.currentTarget.value)} w={240} />
      </Group>
      {groups.length === 0 && (
        <Text size="sm" c="dimmed">
          {items.length ? 'Nothing matches.' : 'Check folders or cases on the left, or add a filter.'}
        </Text>
      )}
      {groups.map((g) => {
        const showAll = expanded.has(g.id);
        const shown = showAll ? g.list : g.list.slice(0, SHOWN);
        return (
          <Card key={g.id || 'none'} withBorder radius="md" padding={0} data-testid="plan-group">
            <Group justify="space-between" gap="xs" wrap="wrap" p="xs" style={{ borderBottom: '1px solid var(--fh-ui-hairline)', background: 'var(--fh-ui-surface-2, transparent)' }}>
              <Group gap={6} wrap="nowrap" style={{ minWidth: 0 }}>
                <IconFolder size={14} aria-hidden />
                <Text size="sm" fw={600} className="fh-truncate" component="h3" m={0}>
                  {g.label}
                </Text>
                <Badge size="xs" variant="default" radius="sm">
                  {g.list.length}
                </Badge>
              </Group>
              {!readOnly && <FolderAssign people={people} folder={g.label} onApply={(v) => run({ assign: g.list.map((i) => ({ number: i.caseNumber, assignee: v })) }, v ? `Assigned to ${v}` : 'Unassigned')} />}
            </Group>
            {shown.map((it) => (
              <Group key={it.caseNumber} gap="xs" wrap="wrap" px="xs" py={6} style={{ borderBottom: '1px solid var(--fh-ui-hairline)' }} data-testid="plan-item" data-case={it.caseNumber}>
                <AutomationIcon automated={it.automated} size={14} />
                <Anchor component={Link} to={caseLink(project, it.caseNumber)} size="sm" style={{ flex: '1 1 200px', minWidth: 0 }} className="fh-truncate">
                  <Text span size="xs" c="dimmed" className="fp-mono" mr={6}>
                    {caseLabel(it.caseNumber)}
                  </Text>
                  {it.name}
                </Anchor>
                {it.viaFilter && (
                  <Badge size="xs" variant="outline" color="gray" radius="sm">
                    filter
                  </Badge>
                )}
                {it.automated ? (
                  <Text size="xs" c="dimmed" w={210}>
                    runs in CI
                  </Text>
                ) : readOnly ? (
                  <Text size="xs" w={210}>
                    {it.assignee ?? 'Unassigned'}
                  </Text>
                ) : (
                  <AssigneeInput key={`${String(it.caseNumber)}:${it.assignee ?? ''}`} value={it.assignee ?? null} people={people} label={`Assignee of ${caseLabel(it.caseNumber)}`} onCommit={(v) => run({ assign: [{ number: it.caseNumber, assignee: v }] }, v ? `Assigned to ${v}` : 'Unassigned')} testId="plan-item-assignee" />
                )}
                {!readOnly && (
                  <Tooltip label="Remove from the plan">
                    <ActionIcon variant="subtle" color="red" size="sm" aria-label={`Remove ${caseLabel(it.caseNumber)} from the plan`} onClick={() => run({ remove: [it.caseNumber] }, 'Removed')}>
                      <IconX size={14} />
                    </ActionIcon>
                  </Tooltip>
                )}
              </Group>
            ))}
            {g.list.length > SHOWN && (
              <Button variant="subtle" size="xs" fullWidth onClick={() => setExpanded((p) => new Set(p).add(g.id))} disabled={showAll}>
                {showAll ? `All ${String(g.list.length)} shown` : `Show all ${String(g.list.length)}`}
              </Button>
            )}
          </Card>
        );
      })}
    </Stack>
  );
}
