import { EmptyState, useCan } from '@franshub/ui-shell';
import { ActionIcon, Alert, Badge, Button, Card, Group, Menu, Modal, NumberInput, Select, Stack, Text, TextInput, Textarea, Tooltip } from '@mantine/core';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconArrowDown, IconArrowUp, IconCopy, IconLink, IconLock, IconLockOpen, IconPencil, IconPlus, IconSettings, IconTrash, IconX } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { Link, useNavigate, useParams, useSearchParams } from 'react-router';
import { errorMessage } from '../../api/client';
import {
  GRID_COLUMNS,
  MAX_WIDGETS,
  WIDGET_CATALOG,
  WIDGET_TYPES,
  newWidget,
  packWidgets,
  useCloneDashboard,
  useDashboardData,
  useDashboardDoc,
  useDeleteDashboard,
  useLockDashboard,
  useSaveDashboard,
  useWidgetPreview,
  type DashboardDoc,
  type DashboardWidget,
  type WidgetResult,
  type WidgetSettings,
  type WidgetType,
} from '../../api/dashboards';
import { useProjects } from '../../api/hooks';
import { isConflict } from '../../api/tm';
import { CopyValue } from '../../components/CopyButton';
import { PageHeader } from '../../components/PageHeader';
import { QueryState } from '../../components/QueryState';
import { WidgetBody } from './widgets';

const ROW = 72;
const WIDTHS = [3, 4, 6, 8, 9, 12];
const HEIGHTS = [3, 4, 5, 6, 8, 10];

function WidgetCard({ project, w, result, editing, first, last, onMove, onChange, onRemove, onSettings }: { project: string; w: DashboardWidget; result?: WidgetResult; editing: boolean; first: boolean; last: boolean; onMove: (d: -1 | 1) => void; onChange: (p: Partial<DashboardWidget>) => void; onRemove: () => void; onSettings: () => void }) {
  const titleId = `wt-${w.id}`;
  return (
    <Card withBorder radius="md" padding="md" component="section" aria-labelledby={titleId} data-testid={`widget-${w.id}`} style={{ minWidth: 0, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      <Group justify="space-between" wrap="nowrap" mb="xs" gap="xs">
        <Text id={titleId} fw={600} size="sm" className="fh-truncate" style={{ minWidth: 0 }}>
          {w.title || WIDGET_CATALOG[w.type].title}
        </Text>
        {editing && (
          <Group gap={2} wrap="nowrap" style={{ flex: 'none' }}>
            <Tooltip label="Move earlier">
              <ActionIcon variant="subtle" size="sm" disabled={first} onClick={() => onMove(-1)} aria-label={`Move ${w.title} earlier`}>
                <IconArrowUp size={14} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label="Move later">
              <ActionIcon variant="subtle" size="sm" disabled={last} onClick={() => onMove(1)} aria-label={`Move ${w.title} later`}>
                <IconArrowDown size={14} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label="Settings">
              <ActionIcon variant="subtle" size="sm" onClick={onSettings} aria-label={`Settings of ${w.title}`}>
                <IconSettings size={14} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label="Remove">
              <ActionIcon variant="subtle" color="red" size="sm" onClick={onRemove} aria-label={`Remove ${w.title}`}>
                <IconTrash size={14} />
              </ActionIcon>
            </Tooltip>
          </Group>
        )}
      </Group>
      {editing && (
        <Group gap="xs" mb="xs">
          <Select size="xs" w={96} aria-label={`Width of ${w.title}`} data={WIDTHS.map((n) => ({ value: String(n), label: `${String(n)} of ${String(GRID_COLUMNS)}` }))} value={String(w.w)} allowDeselect={false} onChange={(v) => v && onChange({ w: Number(v) })} />
          <Select size="xs" w={88} aria-label={`Height of ${w.title}`} data={HEIGHTS.map((n) => ({ value: String(n), label: `${String(n)} rows` }))} value={String(w.h)} allowDeselect={false} onChange={(v) => v && onChange({ h: Number(v) })} />
        </Group>
      )}
      <div style={{ flex: 1, minHeight: 0, overflow: 'auto' }}>
        {!result ? (
          <Text size="sm" c="dimmed">
            Loading…
          </Text>
        ) : result.status === 'error' ? (
          <Alert color="red" variant="light" p="xs" title="Widget unavailable">
            {result.message}
          </Alert>
        ) : (
          <WidgetBody project={project} widget={w} data={result.data} />
        )}
      </div>
    </Card>
  );
}

function SettingsModal({ widget, onClose, onSave }: { widget: DashboardWidget; onClose: () => void; onSave: (title: string, s: WidgetSettings) => void }) {
  const info = WIDGET_CATALOG[widget.type];
  const [title, setTitle] = useState(widget.title);
  const [s, setS] = useState<WidgetSettings>(widget.settings);
  const has = (k: keyof WidgetSettings) => info.settings.includes(k);
  const set = (p: Partial<WidgetSettings>) => setS((prev) => ({ ...prev, ...p }));
  return (
    <Modal opened onClose={onClose} title={`${info.title}: settings`} styles={{ title: { fontWeight: 700 } }}>
      <Stack gap="sm">
        <Text size="sm" c="dimmed">
          {info.description}
        </Text>
        <TextInput label="Title" value={title} maxLength={120} onChange={(e) => setTitle(e.currentTarget.value)} />
        {has('days') && <NumberInput label="Days back" description="1 to 730" min={1} max={730} allowDecimal={false} value={s.days ?? 30} onChange={(v) => set({ days: typeof v === 'number' ? v : undefined })} />}
        {has('limit') && <NumberInput label="Items" description="1 to 50 (lists show at most 10)" min={1} max={50} allowDecimal={false} value={s.limit ?? 10} onChange={(v) => set({ limit: typeof v === 'number' ? v : undefined })} />}
        {has('bucket') && <Select label="Bucket" data={[{ value: 'day', label: 'Day' }, { value: 'week', label: 'Week' }]} value={s.bucket ?? 'day'} allowDeselect={false} onChange={(v) => set({ bucket: v === 'week' ? 'week' : 'day' })} />}
        {has('chart') && <Select label="Chart" data={[{ value: 'bar', label: 'Stacked bars' }, { value: 'line', label: 'Lines' }, { value: 'area', label: 'Stacked areas' }]} value={s.chart ?? 'bar'} allowDeselect={false} onChange={(v) => set({ chart: v === 'line' || v === 'area' ? v : 'bar' })} />}
        {has('view') && <Select label="View" data={[{ value: 'chart', label: 'Chart' }, { value: 'table', label: 'Table' }]} value={s.view ?? 'chart'} allowDeselect={false} onChange={(v) => set({ view: v === 'table' ? 'table' : 'chart' })} />}
        {has('groupBy') && <TextInput label="Label to group by" description="A label name such as suite, feature or owner" maxLength={64} value={s.groupBy ?? ''} placeholder="suite" onChange={(e) => set({ groupBy: e.currentTarget.value || undefined })} />}
        {has('attribute') && <TextInput label="Launch attribute" description="The attribute name to group launches by, such as build or release" maxLength={64} value={s.attribute ?? ''} placeholder="build" onChange={(e) => set({ attribute: e.currentTarget.value.trim() || undefined })} />}
        {has('branch') && <TextInput label="Branch" maxLength={200} value={s.branch ?? ''} placeholder="All branches" onChange={(e) => set({ branch: e.currentTarget.value || undefined })} />}
        {has('environment') && <TextInput label="Environment" maxLength={200} value={s.environment ?? ''} placeholder="All environments" onChange={(e) => set({ environment: e.currentTarget.value || undefined })} />}
        {has('launchId') && <TextInput label="Launch id" description="Empty means the latest launch" maxLength={36} value={s.launchId ?? ''} onChange={(e) => set({ launchId: e.currentTarget.value.trim() || undefined })} />}
        <Group justify="flex-end" gap="xs">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={() => onSave(title.trim() || info.title, s)}>Apply</Button>
        </Group>
      </Stack>
    </Modal>
  );
}

function CloneModal({ project, id, name, onClose }: { project: string; id: string; name: string; onClose: () => void }) {
  const projects = useProjects();
  const clone = useCloneDashboard(project, id);
  const navigate = useNavigate();
  const [target, setTarget] = useState(project);
  const [title, setTitle] = useState(`${name} (copy)`.slice(0, 120));
  return (
    <Modal opened onClose={onClose} title="Clone dashboard" styles={{ title: { fontWeight: 700 } }}>
      <Stack gap="sm">
        <TextInput label="Name" value={title} maxLength={120} onChange={(e) => setTitle(e.currentTarget.value)} />
        <Select label="Project" description="You need to be allowed to change dashboards in the target project" data={(projects.data ?? []).map((p) => ({ value: p.key, label: p.name }))} value={target} allowDeselect={false} searchable onChange={(v) => v && setTarget(v)} />
        <Group justify="flex-end" gap="xs">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
          <Button
            disabled={!title.trim()}
            loading={clone.isPending}
            onClick={() =>
              clone.mutate(
                { name: title.trim(), targetProject: target },
                {
                  onSuccess: (d) => {
                    notifications.show({ color: 'green', message: 'Dashboard cloned' });
                    void navigate(`/p/${target}/dashboards/${d.id}`);
                  },
                  onError: (e) => notifications.show({ color: 'red', message: errorMessage(e) }),
                },
              )
            }
          >
            Clone
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}

function View({ project, doc, etag }: { project: string; doc: DashboardDoc; etag: string }) {
  const [sp, setSp] = useSearchParams();
  const navigate = useNavigate();
  const canWrite = useCan('dashboards:write', project);
  const isManager = useCan('settings:write', project);
  const wide = useMediaQuery('(min-width: 62em)', true);
  const owner = doc.owned;
  const canEdit = canWrite && (!doc.locked || owner || isManager);
  const canLock = canWrite && (owner || isManager);
  const editing = sp.get('edit') === '1' && canEdit;
  const [draft, setDraft] = useState<{ name: string; description: string; widgets: DashboardWidget[] } | null>(null);
  const [settingsOf, setSettingsOf] = useState<string | null>(null);
  const [cloning, setCloning] = useState(false);
  const [conflict, setConflict] = useState<string | null>(null);
  const live = useDashboardData(project, doc.id, !editing);
  const save = useSaveDashboard(project, doc.id);
  const lock = useLockDashboard(project, doc.id);
  const del = useDeleteDashboard(project);
  const current = editing ? (draft ?? { name: doc.name, description: doc.description, widgets: doc.widgets }) : { name: doc.name, description: doc.description, widgets: doc.widgets };
  const preview = useWidgetPreview(project, current.widgets, editing);
  const data = editing ? preview.data : live.data;
  const widgets = useMemo(() => packWidgets(current.widgets), [current.widgets]);
  const link = typeof window === 'undefined' ? '' : `${window.location.origin}/p/${project}/dashboards/${doc.id}`;

  const setEdit = (on: boolean) =>
    setSp(
      (prev) => {
        const n = new URLSearchParams(prev);
        if (on) n.set('edit', '1');
        else n.delete('edit');
        return n;
      },
      { replace: true },
    );
  const update = (fn: (ws: DashboardWidget[]) => DashboardWidget[]) => setDraft({ ...current, widgets: packWidgets(fn(current.widgets)) });
  const patch = (id: string, p: Partial<DashboardWidget>) => update((ws) => ws.map((w) => (w.id === id ? { ...w, ...p } : w)));
  const move = (i: number, d: -1 | 1) =>
    update((ws) => {
      const out = [...ws];
      const [x] = out.splice(i, 1);
      if (x) out.splice(i + d, 0, x);
      return out;
    });
  const add = (type: WidgetType) => update((ws) => [...ws, newWidget(type, ws)]);

  const onSave = () =>
    save.mutate(
      { etag, body: { name: current.name.trim() || doc.name, description: current.description, widgets } },
      {
        onSuccess: () => {
          setDraft(null);
          setConflict(null);
          setEdit(false);
          notifications.show({ color: 'green', message: 'Dashboard saved' });
        },
        onError: (e) => {
          if (isConflict(e)) setConflict(e.conflict.changedBy ? `${e.conflict.changedBy} changed this dashboard after you opened it.` : 'This dashboard changed after you opened it.');
          else notifications.show({ color: 'red', message: errorMessage(e) });
        },
      },
    );

  const editingWidget = settingsOf ? widgets.find((w) => w.id === settingsOf) : undefined;

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project, to: `/p/${project}` }, { label: 'Dashboards', to: `/p/${project}/dashboards` }, { label: doc.name }]}
        title={editing ? <TextInput aria-label="Dashboard name" value={current.name} maxLength={120} onChange={(e) => setDraft({ ...current, name: e.currentTarget.value })} /> : doc.name}
        badges={
          doc.locked && (
            <Badge variant="light" color="yellow" leftSection={<IconLock size={12} />}>
              Locked
            </Badge>
          )
        }
        subtitle={editing ? <Textarea aria-label="Description" autosize minRows={1} maxRows={3} maxLength={2000} placeholder="Description" value={current.description} onChange={(e) => setDraft({ ...current, description: e.currentTarget.value })} /> : `${doc.description ? `${doc.description}. ` : ''}Owner ${doc.owner}${doc.locked ? ', only the owner and project managers can change it' : ''}`}
        actions={
          <Group gap="xs" wrap="wrap">
            {editing ? (
              <>
                <Menu position="bottom-end" withinPortal>
                  <Menu.Target>
                    <Button variant="default" leftSection={<IconPlus size={16} />} disabled={widgets.length >= MAX_WIDGETS}>
                      Add widget
                    </Button>
                  </Menu.Target>
                  <Menu.Dropdown mah={360} style={{ overflowY: 'auto' }}>
                    {(['Launches', 'Trends', 'Tests', 'Failures', 'Project'] as const).map((g) => (
                      <div key={g}>
                        <Menu.Label>{g}</Menu.Label>
                        {WIDGET_TYPES.filter((t) => WIDGET_CATALOG[t].group === g).map((t) => (
                          <Menu.Item key={t} onClick={() => add(t)}>
                            {WIDGET_CATALOG[t].title}
                          </Menu.Item>
                        ))}
                      </div>
                    ))}
                  </Menu.Dropdown>
                </Menu>
                <Button variant="default" leftSection={<IconX size={16} />} onClick={() => { setDraft(null); setConflict(null); setEdit(false); }}>
                  Cancel
                </Button>
                <Button loading={save.isPending} onClick={onSave}>
                  Save
                </Button>
              </>
            ) : (
              <>
                <Group gap={4} wrap="nowrap">
                  <Tooltip label="Anyone who can read this project opens it with this link">
                    <Text size="xs" c="dimmed" className="fh-truncate" maw={200} data-testid="share-link">
                      <IconLink size={12} style={{ verticalAlign: -1 }} /> {link.replace(/^https?:\/\//, '')}
                    </Text>
                  </Tooltip>
                  <CopyValue value={link} label="Copy link" />
                </Group>
                {canWrite && (
                  <Button variant="default" leftSection={<IconCopy size={16} />} onClick={() => setCloning(true)}>
                    Clone
                  </Button>
                )}
                {canLock && (
                  <Button variant="default" leftSection={doc.locked ? <IconLockOpen size={16} /> : <IconLock size={16} />} loading={lock.isPending} onClick={() => lock.mutate({ locked: !doc.locked, etag }, { onError: (e) => notifications.show({ color: 'red', message: errorMessage(e) }) })}>
                    {doc.locked ? 'Unlock' : 'Lock'}
                  </Button>
                )}
                {canEdit && (
                  <Button leftSection={<IconPencil size={16} />} onClick={() => setEdit(true)}>
                    Edit
                  </Button>
                )}
                {canEdit && (
                  <Button
                    color="red"
                    variant="subtle"
                    leftSection={<IconTrash size={16} />}
                    loading={del.isPending}
                    onClick={() => {
                      if (window.confirm(`Delete the dashboard "${doc.name}"?`)) del.mutate({ id: doc.id, etag }, { onSuccess: () => void navigate(`/p/${project}/dashboards`), onError: (e) => notifications.show({ color: 'red', message: errorMessage(e) }) });
                    }}
                  >
                    Delete
                  </Button>
                )}
              </>
            )}
          </Group>
        }
      />
      {conflict && (
        <Alert color="orange" mb="md" title="Your changes were not saved">
          {conflict}{' '}
          <Button component={Link} to={`/p/${project}/dashboards/${doc.id}`} reloadDocument size="compact-xs" variant="light">
            Reload
          </Button>
        </Alert>
      )}
      {widgets.length === 0 ? (
        <EmptyState title="This dashboard has no widgets" description={canEdit ? 'Use Edit, then Add widget.' : 'The owner has not added widgets yet.'} />
      ) : (
        <div
          data-testid="dashboard-grid"
          style={wide ? { display: 'grid', gridTemplateColumns: `repeat(${String(GRID_COLUMNS)}, minmax(0, 1fr))`, gridAutoRows: `${String(ROW)}px`, gap: 12 } : { display: 'flex', flexDirection: 'column', gap: 12 }}
        >
          {widgets.map((w, i) => (
            <div key={w.id} style={wide ? { gridColumn: `${String(w.x + 1)} / span ${String(w.w)}`, gridRow: `${String(w.y + 1)} / span ${String(w.h)}`, minWidth: 0, display: 'flex', flexDirection: 'column' } : { minHeight: Math.min(w.h, 5) * ROW, display: 'flex', flexDirection: 'column' }}>
              <div style={{ flex: 1, minHeight: 0, display: 'flex', flexDirection: 'column' }}>
                <WidgetCard project={project} w={w} result={data?.widgets[w.id]} editing={editing} first={i === 0} last={i === widgets.length - 1} onMove={(d) => move(i, d)} onChange={(p) => patch(w.id, p)} onRemove={() => update((ws) => ws.filter((x) => x.id !== w.id))} onSettings={() => setSettingsOf(w.id)} />
              </div>
            </div>
          ))}
        </div>
      )}
      <div style={{ marginTop: 16 }}>
        <Text size="xs" c="dimmed">
          {editing ? `${String(widgets.length)} of ${String(MAX_WIDGETS)} widgets. Widgets flow left to right in the order shown.` : data ? `Updated ${new Date(data.generatedAt).toLocaleTimeString()}, refreshes every minute` : ''}
        </Text>
      </div>
      {editingWidget && (
        <SettingsModal
          widget={editingWidget}
          onClose={() => setSettingsOf(null)}
          onSave={(title, settings) => {
            patch(editingWidget.id, { title, settings });
            setSettingsOf(null);
          }}
        />
      )}
      {cloning && <CloneModal project={project} id={doc.id} name={doc.name} onClose={() => setCloning(false)} />}
    </>
  );
}

export default function DashboardViewPage() {
  const { key = '', id = '' } = useParams();
  const q = useDashboardDoc(key, id);
  return (
    <QueryState query={q} notFound="Dashboard not found">
      {() => (q.data ? <View key={`${q.data.value.id}-${String(q.data.value.revision)}`} project={key} doc={q.data.value} etag={q.data.etag} /> : null)}
    </QueryState>
  );
}
