import { Badge, Card, Checkbox, Group, Stack, Text, Tooltip } from '@mantine/core';
import { forwardRef, type HTMLAttributes } from 'react';
import { Link } from 'react-router';
import { Virtuoso } from 'react-virtuoso';
import type { CaseFieldDef, CaseFolder, CaseSummary } from '../../api/tm';
import { formatRelative } from '../../lib/format';
import { caseLabel } from '../../lib/tm/filters';
import { caseLink, folderPathLabel } from '../../lib/tm/ui';
import { STATUS_LABEL, statusVar } from '../../lib/status';
import { StatusDot } from '../../components/Status';
import { AiDraftBadge, AutomationIcon, CaseStatusBadge, ManualIcon } from './common';
import { DRAG_TYPE } from './CaseTree';

function LastResult({ c }: { c: CaseSummary }) {
  const r = c.lastResult;
  if (!r) {
    return (
      <Text size="xs" c="dimmed">
        never run
      </Text>
    );
  }
  const label = (STATUS_LABEL as Record<string, string | undefined>)[r.status] ?? r.status;
  return (
    <Tooltip label={`${label}, ${r.source}, ${formatRelative(r.at)}`}>
      <Group gap={4} wrap="nowrap" data-testid="case-last-result">
        <StatusDot status={r.status} size={8} />
        <Text size="xs" style={{ color: statusVar(r.status) }}>
          {label}
        </Text>
        {r.source === 'manual' && <ManualIcon size={12} />}
      </Group>
    </Tooltip>
  );
}

function shortDate(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

function fieldText(c: CaseSummary, key: string): string {
  const v = c.fields[key];
  if (v === null || v === undefined) return '';
  return Array.isArray(v) ? v.join(', ') : String(v);
}

function priorityColor(defs: readonly CaseFieldDef[], value: string): string | undefined {
  return defs.find((d) => d.key === 'priority')?.options.find((o) => o.value === value)?.color ?? undefined;
}

export interface CaseListProps {
  project: string;
  items: CaseSummary[];
  fields: CaseFieldDef[];
  folders: CaseFolder[] | undefined;
  selected: ReadonlySet<number>;
  onToggle: (n: number, on: boolean) => void;
  previewed: number | null;
  onPreview: (n: number) => void;
  cursor: number;
  setCursor: (i: number) => void;
  onEnd: () => void;
  mobile: boolean;
  draggable: boolean;
}

const List = forwardRef<HTMLDivElement, HTMLAttributes<HTMLDivElement>>(function List(props, ref) {
  return <div {...props} ref={ref} role="list" aria-label="Test cases" />;
});

function Item(props: HTMLAttributes<HTMLDivElement> & { item?: unknown; context?: unknown }) {
  const rest: Record<string, unknown> = { ...props };
  delete rest.item;
  delete rest.context;
  return <div {...rest} role="listitem" />;
}

export function CaseList({ project, items, fields, folders, selected, onToggle, previewed, onPreview, cursor, setCursor, onEnd, mobile, draggable }: CaseListProps) {
  const row = (i: number, c: CaseSummary) => {
    const checked = selected.has(c.number);
    const priority = fieldText(c, 'priority');
    const owner = fieldText(c, 'owner');
    const dragStart = (e: React.DragEvent) => {
      const numbers = checked ? [...selected] : [c.number];
      e.dataTransfer.setData(DRAG_TYPE, JSON.stringify(numbers));
      e.dataTransfer.effectAllowed = 'move';
    };
    const checkbox = <Checkbox size="xs" checked={checked} onChange={(e) => onToggle(c.number, e.currentTarget.checked)} aria-label={`Select ${caseLabel(c.number)} ${c.name}`} data-testid="case-check" />;
    if (mobile) {
      return (
        <div data-list-index={i} data-testid="case-card" data-case={c.number} style={{ paddingBottom: 8 }}>
          <Card withBorder radius="md" padding="sm">
            <Group gap="xs" wrap="nowrap" align="flex-start">
              {checkbox}
              <Stack gap={4} style={{ minWidth: 0, flex: 1 }}>
                <Group gap={6} wrap="nowrap">
                  <AutomationIcon automated={c.automated} size={14} />
                  <Text size="xs" c="dimmed" className="fp-mono">
                    {caseLabel(c.number)}
                  </Text>
                  <CaseStatusBadge status={c.status} size="xs" />
                </Group>
                <Text component={Link} to={caseLink(project, c.number)} size="sm" fw={600} style={{ wordBreak: 'break-word' }} data-testid="case-link">
                  {c.name}
                </Text>
                <Group gap={8} wrap="wrap">
                  {c.aiUnreviewed && <AiDraftBadge />}
                  {priority && (
                    <Text size="xs" c="dimmed">
                      {priority}
                    </Text>
                  )}
                  <LastResult c={c} />
                </Group>
              </Stack>
            </Group>
          </Card>
        </div>
      );
    }
    return (
      <div
        className="fp-row fp-tm-case-row"
        data-selected={i === cursor || previewed === c.number}
        data-list-index={i}
        data-testid="case-row"
        data-case={c.number}
        draggable={draggable}
        onDragStart={dragStart}
        onMouseEnter={() => setCursor(i)}
        onClick={(e) => {
          if ((e.target as HTMLElement).closest('input,a,button')) return;
          onPreview(c.number);
        }}
      >
        {checkbox}
        <Text size="xs" c="dimmed" className="fp-mono" style={{ width: 64, flex: 'none' }}>
          {caseLabel(c.number)}
        </Text>
        <AutomationIcon automated={c.automated} size={15} />
        <Stack gap={0} className="fp-tm-case-name">
          <Group gap={6} wrap="nowrap" style={{ minWidth: 0 }}>
            <Text component={Link} to={caseLink(project, c.number)} size="sm" fw={600} className="fh-truncate" data-testid="case-link">
              {c.name}
            </Text>
            {c.aiUnreviewed && <AiDraftBadge />}
          </Group>
          <Text size="xs" c="dimmed" className="fh-truncate">
            {folderPathLabel(folders, c.folderId ?? null)}
            {c.tags.length ? ` · ${c.tags.join(', ')}` : ''}
          </Text>
        </Stack>
        <div style={{ width: 86, flex: 'none' }}>
          <CaseStatusBadge status={c.status} size="xs" />
        </div>
        <div style={{ width: 72, flex: 'none' }} className="fp-tm-col-md">
          {priority && (
            <Badge size="xs" variant="dot" color={priorityColor(fields, priority) ?? 'gray'} radius="sm">
              {priority}
            </Badge>
          )}
        </div>
        <Text size="xs" c="dimmed" className="fh-truncate fp-tm-col-lg" style={{ width: 130, flex: 'none' }}>
          {owner}
        </Text>
        <div style={{ width: 92, flex: 'none' }} className="fp-tm-col-sm">
          <LastResult c={c} />
        </div>
        <Text size="xs" c="dimmed" className="fp-tm-col-lg" style={{ width: 80, flex: 'none', textAlign: 'right' }}>
          {shortDate(c.updatedAt)}
        </Text>
      </div>
    );
  };

  return <Virtuoso useWindowScroll data={items} computeItemKey={(_i, c) => c.number} itemContent={row} endReached={onEnd} increaseViewportBy={400} components={{ List, Item }} data-testid="case-list" />;
}
