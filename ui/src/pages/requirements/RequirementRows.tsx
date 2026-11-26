import { ActionIcon, Badge, Button, Group, Table, Text, Tooltip } from '@mantine/core';
import { IconArrowBackUp, IconChevronDown, IconChevronRight } from '@tabler/icons-react';
import { Fragment } from 'react';
import { queryErrorOf, requirementKey, useRequirements, useRequirementTree, type RequirementQuery, type RequirementSummary } from '../../api/requirements';
import { errorMessage } from '../../api/client';
import { IssueLink, RequirementLinkText, RequirementStatusBadge } from './common';

export const COLUMNS = 6;

function Note({ children, color }: { children: string; color?: string }) {
  return (
    <Table.Tr>
      <Table.Td colSpan={COLUMNS}>
        <Text size="sm" c={color ?? 'dimmed'}>
          {children}
        </Text>
      </Table.Td>
    </Table.Tr>
  );
}

interface RowProps {
  project: string;
  r: RequirementSummary;
  depth: number;
  expanded?: Set<number>;
  onToggle?: (n: number) => void;
  onRestore?: (n: number) => void;
  canWrite: boolean;
}

function Row({ project, r, depth, expanded, onToggle, onRestore, canWrite }: RowProps) {
  const open = expanded?.has(r.number) ?? false;
  return (
    <Table.Tr data-testid="requirement-row" data-number={r.number}>
      <Table.Td style={{ paddingLeft: 8 + depth * 18 }}>
        <Group gap={4} wrap="nowrap" align="flex-start">
          {onToggle ? (
            r.hasChildren ? (
              <ActionIcon variant="subtle" size="sm" color="gray" aria-expanded={open} aria-label={`${open ? 'Collapse' : 'Expand'} ${requirementKey(r.number)}`} onClick={() => onToggle(r.number)} data-testid="requirement-toggle">
                {open ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
              </ActionIcon>
            ) : (
              <span style={{ width: 22, flex: 'none' }} aria-hidden />
            )
          ) : null}
          <div style={{ minWidth: 0 }}>
            <RequirementLinkText project={project} number={r.number} title={r.title} />
            {r.archived && (
              <Badge size="xs" variant="outline" color="gray" radius="sm" ml={6}>
                archived
              </Badge>
            )}
          </div>
        </Group>
      </Table.Td>
      <Table.Td>
        <Text size="sm">{r.type}</Text>
      </Table.Td>
      <Table.Td>
        <RequirementStatusBadge status={r.status} />
      </Table.Td>
      <Table.Td>
        <IssueLink issueKey={r.issueKey} externalState={r.externalState} />
      </Table.Td>
      <Table.Td ta="right">{r.cases}</Table.Td>
      <Table.Td ta="right">
        {r.archived && canWrite && onRestore && (
          <Tooltip label="Restore">
            <ActionIcon variant="subtle" aria-label={`Restore ${requirementKey(r.number)}`} onClick={() => onRestore(r.number)} data-testid="requirement-restore">
              <IconArrowBackUp size={16} />
            </ActionIcon>
          </Tooltip>
        )}
      </Table.Td>
    </Table.Tr>
  );
}

interface TreeProps {
  project: string;
  parent: number | null;
  depth: number;
  expanded: Set<number>;
  onToggle: (n: number) => void;
  canWrite: boolean;
}

/** The direct children of one requirement, loaded when it is first expanded. */
export function TreeBranch({ project, parent, depth, expanded, onToggle, canWrite }: TreeProps) {
  const q = useRequirementTree(project, parent);
  const items = q.data?.pages.flatMap((p) => p.items) ?? [];
  if (q.isPending) return <Note>Loading…</Note>;
  if (q.isError) return <Note color="red">{errorMessage(q.error)}</Note>;
  return (
    <>
      {items.length === 0 && parent === null && <Note>No requirements yet.</Note>}
      {items.map((r) => (
        <Fragment key={r.number}>
          <Row project={project} r={r} depth={depth} expanded={expanded} onToggle={onToggle} canWrite={canWrite} />
          {expanded.has(r.number) && r.hasChildren && <TreeBranch project={project} parent={r.number} depth={depth + 1} expanded={expanded} onToggle={onToggle} canWrite={canWrite} />}
        </Fragment>
      ))}
      {q.hasNextPage && (
        <Table.Tr>
          <Table.Td colSpan={COLUMNS} style={{ paddingLeft: 8 + depth * 18 }}>
            <Button variant="subtle" size="compact-xs" onClick={() => void q.fetchNextPage()} loading={q.isFetchingNextPage}>
              Load more
            </Button>
          </Table.Td>
        </Table.Tr>
      )}
    </>
  );
}

export function FlatRows({ project, query, canWrite, onRestore, empty }: { project: string; query: RequirementQuery; canWrite: boolean; onRestore: (n: number) => void; empty: string }) {
  const q = useRequirements(project, query);
  const items = q.data?.pages.flatMap((p) => p.items) ?? [];
  if (q.isPending) return <Note>Loading…</Note>;
  if (q.isError) return <Note color="red">{queryErrorOf(q.error) ? 'Fix the query to see the results.' : errorMessage(q.error)}</Note>;
  return (
    <>
      {items.length === 0 && <Note>{empty}</Note>}
      {items.map((r) => (
        <Row key={r.number} project={project} r={r} depth={0} onRestore={onRestore} canWrite={canWrite} />
      ))}
      {q.hasNextPage && (
        <Table.Tr>
          <Table.Td colSpan={COLUMNS}>
            <Button variant="subtle" size="compact-xs" onClick={() => void q.fetchNextPage()} loading={q.isFetchingNextPage}>
              Load more
            </Button>
          </Table.Td>
        </Table.Tr>
      )}
    </>
  );
}
