import { ActionIcon, Badge, Button, Group, Modal, Radio, SegmentedControl, Select, Stack, Text, TextInput, Tooltip } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconArrowDown, IconArrowUp, IconFolder, IconFolderPlus, IconFolderSymlink, IconPencil, IconTrash } from '@tabler/icons-react';
import { useQueries } from '@tanstack/react-query';
import { useMemo, useState, type DragEvent } from 'react';
import { errorMessage, projectPath, request, withQuery } from '../../api/client';
import { tmk, useCaseFolders, useDeleteFolder, useSaveFolder, type CaseFolder, type CaseTree as CaseTreeBody, type CaseTreeNode } from '../../api/tm';
import type { TreeMode } from '../../lib/tm/filters';
import { folderPathLabel } from '../../lib/tm/ui';
import { TreeView, type TreeRow } from './TreeView';

export const DRAG_TYPE = 'application/x-fastproof-cases';
export const FOLDER_DRAG_TYPE = 'application/x-fastproof-folder';

function siblingsOf(folders: readonly CaseFolder[], parentId: string | null): CaseFolder[] {
  return folders.filter((f) => (f.parentId ?? null) === parentId).sort((a, b) => a.position - b.position || a.name.localeCompare(b.name));
}

/** The folder and every folder below it. */
function subtree(folders: readonly CaseFolder[], id: string): Set<string> {
  const out = new Set([id]);
  for (let grew = true; grew; ) {
    grew = false;
    for (const f of folders) {
      if (f.parentId && out.has(f.parentId) && !out.has(f.id)) {
        out.add(f.id);
        grew = true;
      }
    }
  }
  return out;
}

function countsBadge(total: number) {
  return (
    <Badge size="xs" variant="default" radius="sm" style={{ flex: 'none' }} aria-hidden>
      {total}
    </Badge>
  );
}

function buildFolderRows(folders: readonly CaseFolder[], expanded: ReadonlySet<string>): TreeRow[] {
  const children = new Map<string | null, CaseFolder[]>();
  for (const f of folders) {
    const list = children.get(f.parentId ?? null) ?? [];
    list.push(f);
    children.set(f.parentId ?? null, list);
  }
  for (const list of children.values()) list.sort((a, b) => a.position - b.position || a.name.localeCompare(b.name));
  const rows: TreeRow[] = [];
  const walk = (parent: string | null, level: number) => {
    const list = children.get(parent) ?? [];
    list.forEach((f, i) => {
      const has = (children.get(f.id)?.length ?? 0) > 0;
      const open = expanded.has(f.id);
      rows.push({ id: f.id, level, text: f.name, hasChildren: has, expanded: open, setSize: list.length, posInSet: i + 1, extra: countsBadge(f.counts.total), icon: <IconFolder size={14} aria-hidden style={{ flex: 'none' }} />, testId: 'folder-row', draggable: true });
      if (has && open) walk(f.id, level + 1);
    });
  };
  walk(null, 1);
  return rows;
}

function FolderDialog({ project, folder, parent, onClose }: { project: string; folder: CaseFolder | null; parent: CaseFolder | null; onClose: () => void }) {
  const save = useSaveFolder(project);
  const [name, setName] = useState(folder?.name ?? '');
  const title = folder ? `Rename ${folder.name}` : parent ? `New folder in ${parent.name}` : 'New folder';
  const submit = () => {
    const body = folder ? { name: name.trim() } : { name: name.trim(), parentId: parent?.id ?? null };
    save.mutate(
      { id: folder?.id, body },
      {
        onSuccess: () => onClose(),
        onError: (e) => notifications.show({ color: 'red', title: 'Folder not saved', message: errorMessage(e) }),
      },
    );
  };
  return (
    <Modal opened onClose={onClose} title={title}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (name.trim()) submit();
        }}
      >
        <Stack gap="sm">
          <TextInput label="Name" value={name} onChange={(e) => setName(e.currentTarget.value)} data-autofocus maxLength={500} required data-testid="folder-name" />
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" loading={save.isPending} disabled={!name.trim()} data-testid="folder-save">
              Save
            </Button>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}

function MoveFolderDialog({ project, folder, folders, onClose }: { project: string; folder: CaseFolder; folders: readonly CaseFolder[]; onClose: () => void }) {
  const save = useSaveFolder(project);
  const [target, setTarget] = useState<string>(folder.parentId ?? 'root');
  const data = useMemo(() => {
    const own = subtree(folders, folder.id);
    const list = folders.filter((f) => !own.has(f.id)).map((f) => ({ value: f.id, label: folderPathLabel(folders, f.id) }));
    list.sort((a, b) => a.label.localeCompare(b.label));
    return [{ value: 'root', label: 'Top level' }, ...list];
  }, [folders, folder.id]);
  const parentId = target === 'root' ? null : target;
  const unchanged = parentId === (folder.parentId ?? null);
  return (
    <Modal opened onClose={onClose} title={`Move ${folder.name}`} attributes={{ content: { 'data-testid': 'folder-move-dialog' } }}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (unchanged) return;
          save.mutate(
            { id: folder.id, body: { parentId } },
            {
              onSuccess: () => {
                notifications.show({ color: 'green', title: 'Folder moved', message: `${folder.name} → ${parentId ? folderPathLabel(folders, parentId) : 'top level'}` });
                onClose();
              },
              onError: (err) => notifications.show({ color: 'red', title: 'Folder not moved', message: errorMessage(err) }),
            },
          );
        }}
      >
        <Stack gap="sm">
          <Select label="Move into" description="The folder moves with its subfolders and cases" data={data} value={target} onChange={(v) => setTarget(v ?? 'root')} searchable allowDeselect={false} comboboxProps={{ withinPortal: true }} data-autofocus data-testid="folder-move-target" />
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" loading={save.isPending} disabled={unchanged} data-testid="folder-move-confirm">
              Move
            </Button>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}

function DeleteFolderDialog({ project, folder, onClose, onDeleted }: { project: string; folder: CaseFolder; onClose: () => void; onDeleted: () => void }) {
  const del = useDeleteFolder(project);
  const [mode, setMode] = useState<'moveUp' | 'archiveCases'>('moveUp');
  const nonEmpty = folder.counts.total > 0 || folder.hasChildren;
  return (
    <Modal opened onClose={onClose} title={`Delete ${folder.name}`}>
      <Stack gap="sm">
        {nonEmpty ? (
          <Radio.Group value={mode} onChange={(v) => setMode(v === 'archiveCases' ? 'archiveCases' : 'moveUp')} label={`The folder holds ${String(folder.counts.total)} cases${folder.hasChildren ? ' and folders' : ''}.`}>
            <Stack gap={6} mt={6}>
              <Radio value="moveUp" label="Move them to the parent folder" />
              <Radio value="archiveCases" label="Move the cases to the trash" />
            </Stack>
          </Radio.Group>
        ) : (
          <Text size="sm">The folder is empty.</Text>
        )}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
          <Button
            color="red"
            loading={del.isPending}
            data-testid="folder-delete-confirm"
            onClick={() =>
              del.mutate(
                { id: folder.id, mode: nonEmpty ? mode : undefined },
                {
                  onSuccess: () => {
                    onDeleted();
                    onClose();
                  },
                  onError: (e) => notifications.show({ color: 'red', title: 'Folder not deleted', message: errorMessage(e) }),
                },
              )
            }
          >
            Delete
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}

export interface TreeSelection {
  mode: TreeMode;
  id: string | null;
}

export function CaseTree({ project, selection, onSelect, onDropCases }: { project: string; selection: TreeSelection; onSelect: (s: TreeSelection) => void; onDropCases?: (folderId: string, numbers: number[]) => void }) {
  const mode = selection.mode;
  const canWrite = useCan('cases:write', project);
  const folders = useCaseFolders(project);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [active, setActive] = useState<string | null>(selection.id);
  const [dialog, setDialog] = useState<null | { kind: 'new' | 'rename' | 'delete' | 'move' }>(null);
  const saveFolder = useSaveFolder(project);
  const [reordering, setReordering] = useState(false);

  const byId = useMemo(() => new Map((folders.data ?? []).map((f) => [f.id, f] as const)), [folders.data]);

  const expandedPath = useMemo(() => {
    const out = new Set(expanded);
    let cur = selection.mode === 'folder' && selection.id ? byId.get(selection.id) : undefined;
    for (let i = 0; cur?.parentId && i < 20; i++) {
      out.add(cur.parentId);
      cur = byId.get(cur.parentId);
    }
    return out;
  }, [expanded, selection, byId]);

  const lazyIds = useMemo(() => (mode === 'folder' ? [] : [null, ...[...expanded].filter((id) => id.includes('\u001f') || !byId.has(id))]), [mode, expanded, byId]);
  const lazy = useQueries({
    queries: lazyIds.map((parent) => ({
      queryKey: tmk.tree(project, mode, parent),
      queryFn: () => request<CaseTreeBody>(withQuery(`${projectPath(project)}/cases/tree`, { groupBy: mode, parent })).then((r) => r.nodes),
      staleTime: 30_000,
    })),
  });

  const rows = useMemo<TreeRow[]>(() => {
    if (mode === 'folder') return buildFolderRows(folders.data ?? [], expandedPath);
    const byParent = new Map<string | null, CaseTreeNode[] | undefined>();
    lazyIds.forEach((id, i) => byParent.set(id, lazy[i]?.data));
    const out: TreeRow[] = [];
    const walk = (parent: string | null, level: number) => {
      const list = byParent.get(parent) ?? [];
      list.forEach((n, i) => {
        const open = expanded.has(n.id);
        const loading = open && !byParent.get(n.id);
        out.push({ id: n.id, level, text: n.name, hasChildren: n.hasChildren, expanded: open, loading, setSize: list.length, posInSet: i + 1, extra: countsBadge(n.counts.total), testId: 'node-row' });
        if (open) walk(n.id, level + 1);
      });
    };
    walk(null, 1);
    return out;
  }, [mode, folders.data, expandedPath, expanded, lazy, lazyIds]);

  const toggle = (id: string, open: boolean) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (open) next.add(id);
      else next.delete(id);
      return next;
    });

  const activeFolder = mode === 'folder' && active ? (byId.get(active) ?? null) : null;
  const activeSiblings = activeFolder ? siblingsOf(folders.data ?? [], activeFolder.parentId ?? null) : [];
  const activeIndex = activeFolder ? activeSiblings.findIndex((f) => f.id === activeFolder.id) : -1;

  /** Moves the active folder one place among its siblings, renumbering only the positions that change. */
  const reorder = async (dir: -1 | 1) => {
    if (!activeFolder) return;
    const order = [...activeSiblings];
    const j = activeIndex + dir;
    const other = order[j];
    if (activeIndex < 0 || !other) return;
    order[activeIndex] = other;
    order[j] = activeFolder;
    setReordering(true);
    try {
      for (const [pos, f] of order.entries()) if (f.position !== pos) await saveFolder.mutateAsync({ id: f.id, body: { position: pos } });
    } catch (e) {
      notifications.show({ color: 'red', title: 'Order not saved', message: errorMessage(e) });
    } finally {
      setReordering(false);
    }
  };

  const moveFolder = (id: string, parentId: string | null) => {
    const f = byId.get(id);
    if (!f || (f.parentId ?? null) === parentId) return;
    if (parentId && subtree(folders.data ?? [], id).has(parentId)) {
      notifications.show({ color: 'red', title: 'Folder not moved', message: 'A folder cannot move into itself or one of its subfolders.' });
      return;
    }
    saveFolder.mutate(
      { id, body: { parentId } },
      {
        onSuccess: () => notifications.show({ color: 'green', title: 'Folder moved', message: `${f.name} → ${parentId ? folderPathLabel(folders.data, parentId) : 'top level'}` }),
        onError: (e) => notifications.show({ color: 'red', title: 'Folder not moved', message: errorMessage(e) }),
      },
    );
  };

  const onDrop = (id: string, e: DragEvent<HTMLDivElement>) => {
    const folderId = e.dataTransfer.getData(FOLDER_DRAG_TYPE);
    if (folderId) {
      if (folderId !== id) moveFolder(folderId, id);
      return;
    }
    const raw = e.dataTransfer.getData(DRAG_TYPE);
    try {
      const numbers = JSON.parse(raw) as unknown;
      if (Array.isArray(numbers) && numbers.every((n) => typeof n === 'number')) onDropCases?.(id, numbers);
    } catch {
      /* not ours */
    }
  };

  return (
    <Stack gap="xs" data-testid="case-tree">
      <SegmentedControl
        size="xs"
        fullWidth
        value={mode}
        onChange={(v) => {
          setExpanded(new Set());
          onSelect({ mode: v === 'behavior' || v === 'suite' ? v : 'folder', id: null });
        }}
        data={[
          { value: 'folder', label: 'Folders' },
          { value: 'behavior', label: 'Behaviors' },
          { value: 'suite', label: 'Suites' },
        ]}
        aria-label="Group the tree by"
      />
      <Group gap={4} justify="space-between" wrap="nowrap">
        <Button
          size="compact-xs"
          variant={selection.id ? 'subtle' : 'light'}
          onClick={() => onSelect({ mode, id: null })}
          aria-pressed={!selection.id}
          data-testid="tree-all"
        >
          All cases
        </Button>
        {mode === 'folder' && canWrite && (
          <Group gap={2} wrap="nowrap">
            <Tooltip label={activeFolder ? `New folder in ${activeFolder.name}` : 'New folder'}>
              <ActionIcon variant="subtle" size="md" aria-label={activeFolder ? `New folder in ${activeFolder.name}` : 'New folder'} onClick={() => setDialog({ kind: 'new' })} data-testid="folder-new">
                <IconFolderPlus size={16} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label="Rename folder">
              <ActionIcon variant="subtle" size="md" aria-label="Rename folder" disabled={!activeFolder} onClick={() => setDialog({ kind: 'rename' })} data-testid="folder-rename">
                <IconPencil size={16} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label="Move folder…">
              <ActionIcon variant="subtle" size="md" aria-label="Move folder" disabled={!activeFolder} onClick={() => setDialog({ kind: 'move' })} data-testid="folder-move">
                <IconFolderSymlink size={16} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label="Move folder up">
              <ActionIcon variant="subtle" size="md" aria-label="Move folder up" disabled={!activeFolder || activeIndex <= 0 || reordering} onClick={() => void reorder(-1)} data-testid="folder-up">
                <IconArrowUp size={16} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label="Move folder down">
              <ActionIcon variant="subtle" size="md" aria-label="Move folder down" disabled={!activeFolder || activeIndex < 0 || activeIndex >= activeSiblings.length - 1 || reordering} onClick={() => void reorder(1)} data-testid="folder-down">
                <IconArrowDown size={16} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label="Delete folder">
              <ActionIcon variant="subtle" size="md" color="red" aria-label="Delete folder" disabled={!activeFolder} onClick={() => setDialog({ kind: 'delete' })} data-testid="folder-delete">
                <IconTrash size={16} />
              </ActionIcon>
            </Tooltip>
          </Group>
        )}
      </Group>
      <TreeView
        label={mode === 'folder' ? 'Case folders' : mode === 'behavior' ? 'Behaviors' : 'Suites'}
        rows={rows}
        activeId={active}
        selectedId={selection.id}
        onActiveChange={setActive}
        onToggle={toggle}
        onActivate={(id) => onSelect({ mode, id })}
        onDropOn={mode === 'folder' && canWrite ? onDrop : undefined}
        onDragStartRow={
          mode === 'folder' && canWrite
            ? (id, e) => {
                e.dataTransfer.setData(FOLDER_DRAG_TYPE, id);
                e.dataTransfer.effectAllowed = 'move';
              }
            : undefined
        }
        acceptsDrop={(e) => e.dataTransfer.types.includes(DRAG_TYPE) || e.dataTransfer.types.includes(FOLDER_DRAG_TYPE)}
        testId="case-tree-view"
        empty={
          <Text size="sm" c="dimmed" p="xs">
            {folders.isPending || lazy[0]?.isPending ? 'Loading…' : 'No folders yet.'}
          </Text>
        }
      />
      {dialog?.kind === 'new' && <FolderDialog project={project} folder={null} parent={activeFolder} onClose={() => setDialog(null)} />}
      {dialog?.kind === 'move' && activeFolder && <MoveFolderDialog project={project} folder={activeFolder} folders={folders.data ?? []} onClose={() => setDialog(null)} />}
      {dialog?.kind === 'rename' && activeFolder && <FolderDialog project={project} folder={activeFolder} parent={null} onClose={() => setDialog(null)} />}
      {dialog?.kind === 'delete' && activeFolder && (
        <DeleteFolderDialog
          project={project}
          folder={activeFolder}
          onClose={() => setDialog(null)}
          onDeleted={() => {
            if (selection.id === activeFolder.id) onSelect({ mode, id: null });
            setActive(null);
          }}
        />
      )}
    </Stack>
  );
}
