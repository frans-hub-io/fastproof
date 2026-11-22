import { Badge, Group, Stack, Text } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconFolder } from '@tabler/icons-react';
import { useQueries } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import { casesUrl, tmk, useChangeTestPlanItems, type CaseFolder, type CaseList, type CaseSummary, type TestPlanItem } from '../../api/tm';
import { errorMessage, request } from '../../api/client';
import { caseLabel } from '../../lib/tm/filters';
import { AutomationIcon } from './common';
import { TreeView, type TreeRow } from './TreeView';

const MAX_ADD = 10_000;

async function subtreeNumbers(project: string, folderId: string): Promise<number[]> {
  const out: number[] = [];
  let before: string | undefined;
  for (let i = 0; i < 100 && out.length < 50_000; i++) {
    const page = await request<CaseList>(casesUrl(project, { folder: folderId, recursive: true }, { before, limit: 500 }));
    out.push(...page.items.map((c) => c.number));
    if (!page.next) break;
    before = page.next;
  }
  return out;
}

/** The plan builder's case tree: folders and their cases with tri-state checks (a folder means everything in it). */
export function PlanTree({ project, planId, folders, items, readOnly }: { project: string; planId: string; folders: CaseFolder[]; items: TestPlanItem[]; readOnly: boolean }) {
  const change = useChangeTestPlanItems(project, planId);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [active, setActive] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const byId = useMemo(() => new Map(folders.map((f) => [f.id, f] as const)), [folders]);
  const inPlan = useMemo(() => new Set(items.map((i) => i.caseNumber)), [items]);
  const selectedIn = useMemo(() => {
    const counts = new Map<string, number>();
    for (const it of items) {
      let cur = it.folderId ? byId.get(it.folderId) : undefined;
      for (let i = 0; cur && i < 20; i++) {
        counts.set(cur.id, (counts.get(cur.id) ?? 0) + 1);
        cur = cur.parentId ? byId.get(cur.parentId) : undefined;
      }
    }
    return counts;
  }, [items, byId]);

  const expandedFolders = [...expanded].filter((id) => byId.has(id));
  const caseQueries = useQueries({
    queries: expandedFolders.map((id) => ({
      queryKey: [...tmk.cases(project, { folder: id, recursive: false }), 'plan-tree'],
      queryFn: () => request<CaseList>(casesUrl(project, { folder: id, recursive: false }, { limit: 500 })).then((r) => r.items),
      staleTime: 30_000,
    })),
  });
  const casesOf = useMemo(() => {
    const m = new Map<string, CaseSummary[] | undefined>();
    expandedFolders.forEach((id, i) => m.set(id, caseQueries[i]?.data));
    return m;
  }, [expandedFolders, caseQueries]);

  const rows = useMemo<TreeRow[]>(() => {
    const children = new Map<string | null, CaseFolder[]>();
    for (const f of folders) {
      const list = children.get(f.parentId ?? null) ?? [];
      list.push(f);
      children.set(f.parentId ?? null, list);
    }
    for (const list of children.values()) list.sort((a, b) => a.position - b.position || a.name.localeCompare(b.name));
    const out: TreeRow[] = [];
    const walk = (parent: string | null, level: number) => {
      const subs = children.get(parent) ?? [];
      const cases = parent ? (casesOf.get(parent) ?? []) : [];
      const size = subs.length + cases.length;
      subs.forEach((f, i) => {
        const sel = selectedIn.get(f.id) ?? 0;
        const open = expanded.has(f.id);
        out.push({
          id: f.id,
          level,
          text: f.name,
          hasChildren: f.counts.total > 0 || (children.get(f.id)?.length ?? 0) > 0,
          expanded: open,
          loading: busy === f.id || (open && !casesOf.get(f.id)),
          checked: f.counts.total > 0 && sel >= f.counts.total ? true : sel > 0 ? 'mixed' : false,
          setSize: size,
          posInSet: i + 1,
          icon: <IconFolder size={14} aria-hidden style={{ flex: 'none' }} />,
          extra: (
            <Badge size="xs" variant="default" radius="sm" style={{ flex: 'none' }} aria-hidden>
              {sel}/{f.counts.total}
            </Badge>
          ),
          testId: 'plan-tree-folder',
        });
        if (open) walk(f.id, level + 1);
      });
      cases.forEach((c, i) => {
        out.push({
          id: `case:${String(c.number)}`,
          level,
          text: `${caseLabel(c.number)} ${c.name}`,
          hasChildren: false,
          expanded: false,
          loading: busy === `case:${String(c.number)}`,
          checked: inPlan.has(c.number),
          setSize: size,
          posInSet: subs.length + i + 1,
          icon: <AutomationIcon automated={c.automated} size={13} />,
          testId: 'plan-tree-case',
        });
      });
    };
    walk(null, 1);
    return out;
  }, [folders, casesOf, selectedIn, expanded, busy, inPlan]);

  const toggle = (id: string, open: boolean) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (open) next.add(id);
      else next.delete(id);
      return next;
    });

  const check = async (id: string) => {
    if (readOnly || busy) return;
    setBusy(id);
    try {
      if (id.startsWith('case:')) {
        const n = Number(id.slice(5));
        await change.mutateAsync(inPlan.has(n) ? { remove: [n] } : { add: [n] });
      } else {
        const f = byId.get(id);
        if (!f) return;
        const sel = selectedIn.get(id) ?? 0;
        if (sel >= f.counts.total && sel > 0) {
          const sub = new Set<string>([id]);
          let grew = true;
          while (grew) {
            grew = false;
            for (const x of folders) {
              if (x.parentId && sub.has(x.parentId) && !sub.has(x.id)) {
                sub.add(x.id);
                grew = true;
              }
            }
          }
          const remove = items.filter((i) => i.folderId && sub.has(i.folderId)).map((i) => i.caseNumber);
          for (let i = 0; i < remove.length; i += MAX_ADD) await change.mutateAsync({ remove: remove.slice(i, i + MAX_ADD) });
        } else {
          const add = (await subtreeNumbers(project, id)).filter((n) => !inPlan.has(n));
          for (let i = 0; i < add.length; i += MAX_ADD) await change.mutateAsync({ add: add.slice(i, i + MAX_ADD) });
        }
      }
    } catch (e) {
      notifications.show({ color: 'red', title: 'Plan not changed', message: errorMessage(e) });
    } finally {
      setBusy(null);
    }
  };

  return (
    <Stack gap={4} data-testid="plan-tree">
      <Group justify="space-between">
        <Text size="sm" fw={600}>
          Cases
        </Text>
        <Text size="xs" c="dimmed">
          Space or click checks; a folder means all its cases
        </Text>
      </Group>
      <TreeView
        label="Cases to include in the plan"
        rows={rows}
        activeId={active}
        onActiveChange={setActive}
        onToggle={toggle}
        onCheck={readOnly ? undefined : (id) => void check(id)}
        onActivate={(id) => {
          const r = rows.find((x) => x.id === id);
          if (r?.hasChildren) toggle(id, !r.expanded);
        }}
        testId="plan-tree-view"
        empty={
          <Text size="sm" c="dimmed">
            No folders.
          </Text>
        }
      />
    </Stack>
  );
}
