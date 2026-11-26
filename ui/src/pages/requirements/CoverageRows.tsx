import { ActionIcon, Anchor, Button, Group, Table, Text } from '@mantine/core';
import { IconChevronDown, IconChevronRight, IconListDetails } from '@tabler/icons-react';
import { Fragment, useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { requirementKey, useCoverage, useRequirementCases, type CoverageScope, type RequirementCoverage } from '../../api/requirements';
import { caseLabel } from '../../lib/tm/filters';
import { ChangedAfterRun, CountsBar, CountsText, CoverageChip } from './coverage';
import { LastResultChip, RequirementLinkText } from './common';

const COLUMNS = 4;

function Note({ children, color, indent = 0 }: { children: string; color?: string; indent?: number }) {
  return (
    <Table.Tr>
      <Table.Td colSpan={COLUMNS} style={{ paddingLeft: 8 + indent * 18 }}>
        <Text size="sm" c={color ?? 'dimmed'}>
          {children}
        </Text>
      </Table.Td>
    </Table.Tr>
  );
}

function CaseRows({ project, number, indent }: { project: string; number: number; indent: number }) {
  const q = useRequirementCases(project, number, 'active');
  const items = q.data?.pages.flatMap((p) => p.items) ?? [];
  if (q.isPending) return <Note indent={indent}>Loading cases…</Note>;
  if (q.isError) return <Note color="red" indent={indent}>{errorMessage(q.error)}</Note>;
  if (items.length === 0) return <Note indent={indent}>No active case is linked.</Note>;
  return (
    <>
      {items.map((l) => (
        <Table.Tr key={l.caseNumber} data-testid="coverage-case-row" style={{ background: 'color-mix(in srgb, var(--fp-status-skipped) 6%, transparent)' }}>
          <Table.Td colSpan={2} style={{ paddingLeft: 8 + indent * 18, overflowWrap: 'anywhere' }}>
            <Anchor component={Link} to={`/p/${project}/cases/${String(l.caseNumber)}`} size="sm">
              <span className="fp-mono">{caseLabel(l.caseNumber)}</span> {l.name}
            </Anchor>
          </Table.Td>
          <Table.Td colSpan={2}>
            <LastResultChip result={l.lastResult} />
          </Table.Td>
        </Table.Tr>
      ))}
    </>
  );
}

function Row({ project, r, depth, open, onToggle, showCases, onToggleCases }: { project: string; r: RequirementCoverage; depth: number; open: boolean; onToggle: () => void; showCases: boolean; onToggleCases: () => void }) {
  return (
    <Table.Tr data-testid="coverage-row" data-number={r.number} data-coverage={r.coverage}>
      <Table.Td style={{ paddingLeft: 8 + depth * 18 }}>
        <Group gap={4} wrap="nowrap" align="flex-start">
          {r.hasChildren ? (
            <ActionIcon variant="subtle" size="sm" color="gray" aria-expanded={open} aria-label={`${open ? 'Collapse' : 'Expand'} ${requirementKey(r.number)}`} onClick={onToggle} data-testid="coverage-toggle">
              {open ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
            </ActionIcon>
          ) : (
            <span style={{ width: 22, flex: 'none' }} aria-hidden />
          )}
          <div style={{ minWidth: 0, overflowWrap: 'anywhere' }}>
            <RequirementLinkText project={project} number={r.number} title={r.title} />
            {r.changedAfterRun && (
              <div>
                <ChangedAfterRun />
              </div>
            )}
          </div>
        </Group>
      </Table.Td>
      <Table.Td style={{ whiteSpace: 'nowrap' }} data-testid="coverage-cell">
        <CoverageChip status={r.coverage} />
        <Text hiddenFrom="sm" component="div" mt={2}>
          <CountsText counts={r.cases} />
        </Text>
      </Table.Td>
      <Table.Td visibleFrom="sm" style={{ minWidth: 150 }}>
        <CountsBar counts={r.cases} />
        <CountsText counts={r.cases} />
      </Table.Td>
      <Table.Td ta="right">
        <ActionIcon variant={showCases ? 'light' : 'subtle'} size="sm" color="gray" aria-pressed={showCases} aria-label={`${showCases ? 'Hide' : 'Show'} cases of ${requirementKey(r.number)}`} onClick={onToggleCases} data-testid="coverage-cases-toggle">
          <IconListDetails size={14} />
        </ActionIcon>
      </Table.Td>
    </Table.Tr>
  );
}

/** One level of requirements with their coverage; a row expands into its children or its cases. */
export function CoverageBranch({ project, scope, parent, depth }: { project: string; scope: CoverageScope; parent: number | null; depth: number }) {
  const q = useCoverage(project, scope, parent);
  const [open, setOpen] = useState<Set<number>>(new Set());
  const [cases, setCases] = useState<Set<number>>(new Set());
  const flip = (set: Set<number>, n: number) => {
    const next = new Set(set);
    if (!next.delete(n)) next.add(n);
    return next;
  };
  const items = q.data?.pages.flatMap((p) => p.items) ?? [];
  if (q.isPending) return <Note indent={depth}>Loading…</Note>;
  if (q.isError) return <Note color="red" indent={depth}>{errorMessage(q.error)}</Note>;
  return (
    <>
      {items.length === 0 && <Note indent={depth}>{parent === null ? 'No requirements yet.' : 'No child requirements.'}</Note>}
      {items.map((r) => (
        <Fragment key={r.number}>
          <Row project={project} r={r} depth={depth} open={open.has(r.number)} onToggle={() => setOpen((s) => flip(s, r.number))} showCases={cases.has(r.number)} onToggleCases={() => setCases((s) => flip(s, r.number))} />
          {cases.has(r.number) && <CaseRows project={project} number={r.number} indent={depth + 1} />}
          {open.has(r.number) && r.hasChildren && <CoverageBranch project={project} scope={scope} parent={r.number} depth={depth + 1} />}
        </Fragment>
      ))}
      {q.hasNextPage && (
        <Table.Tr>
          <Table.Td colSpan={COLUMNS} style={{ paddingLeft: 8 + depth * 18 }}>
            <Button variant="subtle" size="compact-xs" onClick={() => void q.fetchNextPage()} loading={q.isFetchingNextPage} data-testid="coverage-more">
              Load more
            </Button>
          </Table.Td>
        </Table.Tr>
      )}
    </>
  );
}
