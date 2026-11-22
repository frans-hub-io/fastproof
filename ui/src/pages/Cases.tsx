import { Anchor, Button, Checkbox, Drawer, Group, Menu, Modal, Select, Stack, Tabs, Text, TextInput } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconChevronDown, IconFileExport, IconFileImport, IconFolders, IconLinkOff, IconListDetails, IconPlus } from '@tabler/icons-react';
import { useCallback, useMemo, useState } from 'react';
import { Link, useNavigate, useParams, useSearchParams } from 'react-router';
import { errorMessage } from '../api/client';
import { flatItems, useProject } from '../api/hooks';
import { createCaseExport, exportCasesUrl, useBulkCases, useCaseFields, useCaseFolders, useCases, useCreateCase } from '../api/tm';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { useListKeys } from '../lib/keys';
import { nodeFields, readListState, toCaseFilter, toCaseQuery, writeListState, type CaseListState, type TreeMode } from '../lib/tm/filters';
import { caseLink, folderPathLabel } from '../lib/tm/ui';
import { BulkBar } from './cases/BulkBar';
import { CaseFilters } from './cases/CaseFilters';
import { CaseList } from './cases/CaseList';
import { CasePreview } from './cases/CasePreview';
import { CaseTree, type TreeSelection } from './cases/CaseTree';
import { UnlinkedTests } from './cases/UnlinkedTests';

function NewCaseDialog({ project, folderId, onClose }: { project: string; folderId: string | null; onClose: () => void }) {
  const folders = useCaseFolders(project);
  const create = useCreateCase(project);
  const navigate = useNavigate();
  const [name, setName] = useState('');
  const [folder, setFolder] = useState<string | null>(folderId);
  const data = useMemo(() => (folders.data ?? []).map((f) => ({ value: f.id, label: folderPathLabel(folders.data, f.id) })).sort((a, b) => a.label.localeCompare(b.label)), [folders.data]);
  return (
    <Modal opened onClose={onClose} title="New test case">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (!name.trim()) return;
          create.mutate(
            { name: name.trim(), folderId: folder, status: 'draft', body: { description: '', preconditions: '', steps: [], links: [] } },
            {
              onSuccess: (c) => void navigate(`${caseLink(project, c.number)}?edit=1`),
              onError: (err) => notifications.show({ color: 'red', title: 'Case not created', message: errorMessage(err) }),
            },
          );
        }}
      >
        <Stack gap="sm">
          <TextInput label="Name" value={name} onChange={(e) => setName(e.currentTarget.value)} required maxLength={500} data-autofocus data-testid="new-case-name" />
          <Select label="Folder" data={data} value={folder} onChange={setFolder} searchable clearable comboboxProps={{ withinPortal: true }} data-testid="new-case-folder" />
          <Text size="xs" c="dimmed">
            The case starts as a draft; add steps on its page.
          </Text>
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" loading={create.isPending} disabled={!name.trim()} data-testid="new-case-create">
              Create
            </Button>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}

export default function CasesPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const [sp, setSp] = useSearchParams();
  const navigate = useNavigate();
  const state = useMemo(() => readListState(sp), [sp]);
  const mode: TreeMode = sp.get('tree') === 'behavior' || sp.get('tree') === 'suite' ? (sp.get('tree') as TreeMode) : 'folder';
  const node = sp.get('node');
  const tab = sp.get('tab') === 'unlinked' ? 'unlinked' : 'cases';
  const wide = useMediaQuery('(min-width: 88em)');
  const mobile = useMediaQuery('(max-width: 48em)');
  const canWrite = useCan('cases:write', key);
  const canImport = useCan('cases:import', key);
  const fields = useCaseFields(key);
  const folders = useCaseFolders(key);
  const bulk = useBulkCases(key);
  const [selected, setSelected] = useState<Set<number>>(() => new Set());
  const [cursor, setCursor] = useState(-1);
  const [drawer, setDrawer] = useState(false);
  const [creating, setCreating] = useState(false);
  const previewed = Number(sp.get('preview')) || null;

  const query = useMemo(() => {
    const q = toCaseQuery(state);
    if (mode !== 'folder' && node) q.field = [...(q.field ?? []), ...nodeFields(mode, node)];
    if (mode !== 'folder') {
      q.folder = undefined;
      q.recursive = undefined;
    }
    return q;
  }, [state, mode, node]);
  const cases = useCases(key, query, tab === 'cases');
  const items = flatItems(cases.data);

  const update = useCallback(
    (s: Partial<CaseListState>, extra?: Record<string, string | null>) => {
      setSp(
        (prev) => {
          const next = writeListState(prev, s);
          for (const [k, v] of Object.entries(extra ?? {})) {
            if (v) next.set(k, v);
            else next.delete(k);
          }
          return next;
        },
        { replace: true },
      );
      setSelected(new Set());
    },
    [setSp],
  );

  const selection: TreeSelection = { mode, id: mode === 'folder' ? state.folder : node };
  const onSelectTree = (s: TreeSelection) => {
    if (s.mode === 'folder') update({ folder: s.id }, { tree: null, node: null });
    else update({ folder: null }, { tree: s.mode, node: s.id });
    setDrawer(false);
  };

  const open = useCallback(
    (i: number) => {
      const c = items[i];
      if (c) void navigate(caseLink(key, c.number));
    },
    [items, navigate, key],
  );
  useListKeys({ count: items.length, index: cursor, setIndex: setCursor, onOpen: open, enabled: tab === 'cases' });

  const preview = (n: number) => {
    if (wide) setSp((prev) => { const next = new URLSearchParams(prev); next.set('preview', String(n)); return next; }, { replace: true });
    else void navigate(caseLink(key, n));
  };

  const toggle = (n: number, on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(n);
      else next.delete(n);
      return next;
    });

  const allOnPage = items.length > 0 && items.every((c) => selected.has(c.number));
  const someOnPage = items.some((c) => selected.has(c.number));

  const dropCases = (folderId: string, numbers: number[]) =>
    bulk.mutate(
      { numbers, action: 'move', value: folderId },
      {
        onSuccess: (r) => {
          notifications.show({ color: 'green', title: 'Moved', message: `${String(r.updated)} cases moved to ${folderPathLabel(folders.data, folderId)}` });
          setSelected(new Set());
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Not moved', message: errorMessage(e) }),
      },
    );

  const exportZip = () =>
    createCaseExport(key, toCaseFilter(state))
      .then((job) => notifications.show({ color: 'green', title: 'Export started', message: job.downloadUrl ? 'The archive is ready.' : 'The archive is being built; it appears in the jobs list.' }))
      .catch((e: unknown) => notifications.show({ color: 'red', title: 'Export failed', message: errorMessage(e) }));

  const setTab = (t: string | null) => setSp((prev) => { const next = new URLSearchParams(prev); if (t === 'unlinked') next.set('tab', 'unlinked'); else next.delete('tab'); return next; }, { replace: true });

  const tree = <CaseTree project={key} selection={selection} onSelect={onSelectTree} onDropCases={canWrite ? dropCases : undefined} />;
  const total = cases.data?.pages[0] ? `${String(items.length)}${cases.hasNextPage ? '+' : ''}` : '…';

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Test cases' }]}
        title="Test cases"
        actions={
          <>
            {canWrite && (
              <Button size="xs" leftSection={<IconPlus size={14} />} onClick={() => setCreating(true)} data-testid="new-case">
                New case
              </Button>
            )}
            {canImport && (
              <Button size="xs" variant="default" component={Link} to={`/p/${key}/cases/import`} leftSection={<IconFileImport size={14} />} data-testid="open-import">
                Import
              </Button>
            )}
            <Menu withinPortal>
              <Menu.Target>
                <Button size="xs" variant="default" leftSection={<IconFileExport size={14} />} rightSection={<IconChevronDown size={12} />}>
                  Export
                </Button>
              </Menu.Target>
              <Menu.Dropdown>
                <Menu.Item component="a" href={exportCasesUrl(key, 'csv', query)} download>
                  CSV (shown cases)
                </Menu.Item>
                <Menu.Item component="a" href={exportCasesUrl(key, 'ndjson', query)} download>
                  NDJSON, full precision
                </Menu.Item>
                <Menu.Item onClick={() => void exportZip()}>Zip with attachments</Menu.Item>
              </Menu.Dropdown>
            </Menu>
          </>
        }
      />
      <Tabs value={tab} onChange={setTab} keepMounted={false}>
        <Tabs.List mb="sm">
          <Tabs.Tab value="cases" leftSection={<IconListDetails size={14} />}>
            Cases
          </Tabs.Tab>
          <Tabs.Tab value="unlinked" leftSection={<IconLinkOff size={14} />} data-testid="tab-unlinked">
            Unlinked tests
          </Tabs.Tab>
        </Tabs.List>
        <Tabs.Panel value="cases">
          <div className="fp-tm-cases" data-wide={wide}>
            {!mobile && <aside className="fp-tm-cases-tree" aria-label="Case tree">{tree}</aside>}
            <section className="fp-tm-cases-list" aria-label="Case list">
              <Stack gap="sm">
                {mobile && (
                  <Button variant="default" size="sm" leftSection={<IconFolders size={16} />} onClick={() => setDrawer(true)} data-testid="open-tree-drawer" h={44}>
                    {selection.id ? (mode === 'folder' ? folderPathLabel(folders.data, selection.id) : selection.id.split('\u001f').join(' / ')) : 'All cases'}
                  </Button>
                )}
                <CaseFilters state={state} fields={fields.data ?? []} onChange={(s) => update(s)} />
                {selected.size > 0 && canWrite && <BulkBar project={key} numbers={[...selected]} fields={fields.data ?? []} archived={state.archived} onDone={() => setSelected(new Set())} />}
                <Group gap="sm" justify="space-between" wrap="nowrap">
                  {canWrite ? (
                    <Checkbox
                      size="xs"
                      label={`Select all ${total} cases`}
                      checked={allOnPage}
                      indeterminate={!allOnPage && someOnPage}
                      onChange={(e) => setSelected(e.currentTarget.checked ? new Set(items.map((c) => c.number)) : new Set())}
                      data-testid="select-all"
                    />
                  ) : (
                    <Text size="xs" c="dimmed">
                      {total} cases
                    </Text>
                  )}
                  <Text size="xs" c="dimmed" visibleFrom="sm">
                    j / k move, Enter opens; drag cases onto a folder or use Move….
                  </Text>
                </Group>
                <QueryState query={cases}>
                  {() =>
                    items.length === 0 ? (
                      <EmptyState title={state.archived ? 'The trash is empty' : 'No cases match'} description={state.archived ? 'Archived cases appear here.' : 'Change the filters or the folder, or create a case.'} />
                    ) : (
                      <CaseList
                        project={key}
                        items={items}
                        fields={fields.data ?? []}
                        folders={folders.data}
                        selected={selected}
                        onToggle={toggle}
                        previewed={previewed}
                        onPreview={preview}
                        cursor={cursor}
                        setCursor={setCursor}
                        onEnd={() => {
                          if (cases.hasNextPage && !cases.isFetchingNextPage) void cases.fetchNextPage();
                        }}
                        mobile={mobile}
                        draggable={canWrite && !mobile}
                      />
                    )
                  }
                </QueryState>
              </Stack>
            </section>
            {wide && (
              <aside className="fp-tm-cases-preview" aria-label="Case preview">
                {previewed ? (
                  <CasePreview project={key} n={previewed} />
                ) : (
                  <Text size="sm" c="dimmed" p="md">
                    Select a case to preview it here, or{' '}
                    <Anchor component={Link} to={`/p/${key}/plans`} underline="always">
                      open the test plans
                    </Anchor>
                    .
                  </Text>
                )}
              </aside>
            )}
          </div>
        </Tabs.Panel>
        <Tabs.Panel value="unlinked">
          <UnlinkedTests project={key} />
        </Tabs.Panel>
      </Tabs>
      <Drawer opened={drawer} onClose={() => setDrawer(false)} title="Folders" size="85%" position="left">
        {tree}
      </Drawer>
      {creating && <NewCaseDialog project={key} folderId={mode === 'folder' ? state.folder : null} onClose={() => setCreating(false)} />}
    </>
  );
}
