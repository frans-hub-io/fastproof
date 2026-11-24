import { Alert, Badge, Button, Group, Menu, Select, Stack, Tabs, Text, TextInput } from '@mantine/core';
import { EmptyState, useCan, useMe } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconArchive, IconArrowBackUp, IconCheck, IconDeviceFloppy, IconDots, IconHistory, IconInfoCircle, IconListDetails, IconPencil, IconPrinter, IconRefresh, IconVersions, IconX } from '@tabler/icons-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate, useParams, useSearchParams } from 'react-router';
import { ApiError, errorMessage, projectPath } from '../api/client';
import { useProject } from '../api/hooks';
import { getVersioned, isConflict, useArchiveCase, useCase, useCaseFields, useCaseFolders, useCaseSuggestions, useReviewCase, useTmEvents, useUpdateCase, type Case as CaseDto, type CaseWrite, type ConflictDetails, type Versioned } from '../api/tm';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatDateTime, formatRelative } from '../lib/format';
import { CASE_STATUSES, CASE_STATUS_LABEL, caseLabel } from '../lib/tm/filters';
import { clearDraft, loadDraft, saveDraft, type CaseDraft } from '../lib/tm/prefs';
import { actorLabel, folderPathLabel } from '../lib/tm/ui';
import { mergeCaseWrite, bothChanged } from '../lib/tm/merge';
import { toWrite } from '../lib/tm/versionDiff';

import { editorAction } from './runner/keymap';
import { FEATURES } from '../features';
import { AiDraftBadge, AutomationIcon, CaseStatusBadge, FromCodeBadge, LiveRegion } from './cases/common';
import { ConflictDialog } from './cases/ConflictDialog';
import { DetailsTab } from './cases/DetailsTab';
import { HistoryTab } from './cases/HistoryTab';
import { MarkdownField } from './cases/MarkdownField';
import { ScenarioView } from './cases/ScenarioView';
import { ParametersTab } from './cases/ParametersTab';
import { RequirementsTab } from './cases/RequirementsTab';
import { ReviewTab } from './cases/ReviewTab';
import { StateChip } from './cases/StateChip';
import { StepsEditor } from './cases/StepsEditor';
import { Suggestions } from './cases/Suggestions';
import { VersionsTab } from './cases/VersionsTab';

const SOURCE_TEXT: Record<string, string> = { code: 'the code sync', ui: 'an edit', ai: 'an accepted AI suggestion', import: 'an import', restore: 'a restore', bulk: 'a bulk change' };

interface Editing {
  draft: CaseWrite;
  base: CaseWrite;
  etag: string;
  revision: number;
}

function CaseBody({ project, projectName, c, etag, reload }: { project: string; projectName: string; c: CaseDto; etag: string; reload: () => Promise<{ data?: Versioned<CaseDto> }> }) {
  const [sp, setSp] = useSearchParams();
  const navigate = useNavigate();
  const canWrite = useCan('cases:write', project);
  const fields = useCaseFields(project);
  const folders = useCaseFolders(project);
  const suggestions = useCaseSuggestions(project, c.number);
  const update = useUpdateCase(project, c.number);
  const review = useReviewCase(project, c.number);
  const archive = useArchiveCase(project, c.number);
  const [editing, setEditing] = useState<Editing | null>(null);
  const [conflict, setConflict] = useState<{ details: ConflictDetails; current: Versioned<CaseDto> | null } | null>(null);
  const [changed, setChanged] = useState<{ revision: number; source: string; by?: string | null } | null>(null);
  const { me } = useMe();
  const [announce, setAnnounce] = useState('');
  const [stored, setStored] = useState<CaseDraft | null>(() => loadDraft(project, c.number));
  const [errors, setErrors] = useState<{ path: string; message: string }[]>([]);
  const titleRef = useRef<HTMLInputElement>(null);
  const tab = sp.get('tab') ?? 'scenario';
  const owned = useMemo(() => new Set(c.codeOwned), [c.codeOwned]);
  const labels = useMemo(() => Object.fromEntries((fields.data ?? []).map((f) => [f.key, f.name])), [fields.data]);

  const start = useCallback(
    (from?: CaseWrite) => {
      const base = toWrite(c);
      setEditing({ draft: from ? structuredClone(from) : structuredClone(base), base, etag, revision: c.revision });
      setErrors([]);
      requestAnimationFrame(() => titleRef.current?.focus());
    },
    [c, etag],
  );

  useEffect(() => {
    if (sp.get('edit') === '1' && canWrite && !editing) {
      start();
      setSp((prev) => { const next = new URLSearchParams(prev); next.delete('edit'); return next; }, { replace: true });
    }
  }, [sp, canWrite, editing, start, setSp]);

  const dirty = !!editing && JSON.stringify(editing.draft) !== JSON.stringify(editing.base);

  useEffect(() => {
    if (!editing || !dirty) return;
    const t = setTimeout(() => saveDraft(project, c.number, { revision: editing.revision, savedAt: new Date().toISOString(), body: editing.draft }), 500);
    return () => clearTimeout(t);
  }, [editing, dirty, project, c.number]);

  useEffect(() => {
    if (!dirty) return;
    const warn = (e: BeforeUnloadEvent) => e.preventDefault();
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty]);

  useTmEvents(project, {
    caseUpdated: (e) => {
      if (e.number !== c.number || e.revision <= c.revision) return;
      setChanged({ revision: e.revision, source: e.source });
      const who = (v: Versioned<CaseDto> | undefined) => {
        if (v && v.value.revision >= e.revision) setChanged((x) => (x && x.revision === e.revision ? { ...x, by: v.value.updatedBy ?? null } : x));
      };
      if (!editing) void reload().then((r) => who(r.data));
      else void getVersioned<CaseDto>(`${projectPath(project)}/cases/${String(c.number)}`).then(who, () => undefined);
    },
  });

  const setDraft = (d: CaseWrite) => setEditing((ed) => (ed ? { ...ed, draft: d } : ed));

  const finish = (saved: Versioned<CaseDto>) => {
    clearDraft(project, c.number);
    setStored(null);
    setEditing(null);
    setConflict(null);
    setChanged(null);
    setErrors([]);
    notifications.show({ color: 'green', title: 'Saved', message: `${caseLabel(saved.value.number)} is now version ${String(saved.value.version)}` });
    setAnnounce(`Saved as version ${String(saved.value.version)}`);
  };

  const save = (body: CaseWrite, withEtag: string) =>
    update.mutate(
      { body, etag: withEtag },
      {
        onSuccess: finish,
        onError: (e) => {
          if (isConflict(e)) {
            setConflict({ details: e.conflict, current: null });
            void getVersioned<CaseDto>(`${projectPath(project)}/cases/${String(c.number)}`).then((cur) => setConflict((x) => (x ? { ...x, current: cur } : x)));
            return;
          }
          if (e instanceof ApiError && e.code === 'invalid_case' && Array.isArray(e.details)) setErrors(e.details as { path: string; message: string }[]);
          notifications.show({ color: 'red', title: 'Not saved', message: e instanceof ApiError && e.code === 'field_from_code' ? 'Some fields come from code and cannot change here.' : errorMessage(e) });
        },
      },
    );

  const onSave = () => {
    if (!editing) return;
    if (!dirty) {
      setEditing(null);
      return;
    }
    save(editing.draft, editing.etag);
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const a = editorAction(e, { inStep: false, released: false });
      if (a?.type !== 'save' || !editing) return;
      e.preventDefault();
      onSave();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  const cancel = () => {
    clearDraft(project, c.number);
    setStored(null);
    setEditing(null);
    setErrors([]);
  };

  const markReviewed = () =>
    review.mutate(
      etag,
      {
        onSuccess: () => {
          notifications.show({ color: 'green', title: 'Marked as reviewed', message: 'The AI draft badge is gone.' });
          setAnnounce('Marked as reviewed');
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Not saved', message: isConflict(e) ? 'The case changed; reload it first.' : errorMessage(e) }),
      },
    );

  const setTab = (t: string | null) => setSp((prev) => { const next = new URLSearchParams(prev); if (!t || t === 'scenario') next.delete('tab'); else next.set('tab', t); return next; }, { replace: true });

  const d = editing?.draft;
  const folderData = useMemo(() => (folders.data ?? []).map((f) => ({ value: f.id, label: folderPathLabel(folders.data, f.id) })).sort((a, b) => a.label.localeCompare(b.label)), [folders.data]);
  const errorFor = (path: string) => errors.find((x) => x.path === path)?.message;

  return (
    <>
      <LiveRegion message={announce} />
      <PageHeader
        crumbs={[
          { label: 'Projects', to: '/' },
          { label: projectName, to: `/p/${project}` },
          { label: 'Test cases', to: `/p/${project}/cases` },
          ...(c.folder?.path ?? []).map((p) => ({ label: p.name, to: `/p/${project}/cases?folder=${p.id}` })),
          { label: caseLabel(c.number) },
        ]}
        title={
          d && !owned.has('name') ? (
            <TextInput ref={titleRef} aria-label="Case name" value={d.name} onChange={(e) => setDraft({ ...d, name: e.currentTarget.value })} size="md" w="min(560px, 100%)" maxLength={500} error={errorFor('name')} data-testid="case-name-input" />
          ) : (
            <span data-testid="case-title">
              <Text span c="dimmed" className="fp-mono" fz="0.7em" mr={8}>
                {caseLabel(c.number)}
              </Text>
              {c.name}
            </span>
          )
        }
        badges={
          <>
            <AutomationIcon automated={c.automated} />
            <CaseStatusBadge status={c.status} />
            <StateChip projectKey={project} caseNumber={c.number} />
            {owned.has('name') && <FromCodeBadge hint="The name comes from the test in code" />}
            {c.aiUnreviewed && <AiDraftBadge />}
            {c.archivedAt && (
              <Badge color="red" variant="light" size="sm" radius="sm">
                in the trash
              </Badge>
            )}
          </>
        }
        subtitle={
          <Text size="xs" c="dimmed" component="span">
            v{c.version} · updated by {c.updatedBy ?? 'unknown'} <span title={formatDateTime(c.updatedAt)}>{formatRelative(c.updatedAt)}</span> · origin {c.origin}
          </Text>
        }
        actions={
          <Group gap="xs" className="fp-no-print">
            {editing ? (
              <>
                <Button size="xs" leftSection={<IconDeviceFloppy size={14} />} onClick={onSave} loading={update.isPending} data-testid="case-save">
                  Save
                </Button>
                <Button size="xs" variant="default" leftSection={<IconX size={14} />} onClick={cancel} data-testid="case-cancel">
                  Cancel
                </Button>
              </>
            ) : (
              canWrite &&
              !c.archivedAt && (
                <Button size="xs" leftSection={<IconPencil size={14} />} onClick={() => start()} data-testid="case-edit">
                  Edit
                </Button>
              )
            )}
            {c.aiUnreviewed && canWrite && !editing && (
              <Button size="xs" variant="light" color="grape" leftSection={<IconCheck size={14} />} onClick={markReviewed} loading={review.isPending} data-testid="case-reviewed">
                Reviewed
              </Button>
            )}
            <Button size="xs" variant="default" leftSection={<IconPrinter size={14} />} onClick={() => window.print()} data-testid="case-print">
              Print
            </Button>
            {canWrite && (
              <Menu withinPortal position="bottom-end">
                <Menu.Target>
                  <Button size="xs" variant="default" aria-label="More actions" px={8}>
                    <IconDots size={14} />
                  </Button>
                </Menu.Target>
                <Menu.Dropdown>
                  {c.archivedAt ? (
                    <Menu.Item leftSection={<IconArrowBackUp size={14} />} onClick={() => archive.mutate(true)}>
                      Restore from the trash
                    </Menu.Item>
                  ) : (
                    <Menu.Item
                      color="red"
                      leftSection={<IconArchive size={14} />}
                      onClick={() =>
                        archive.mutate(false, {
                          onSuccess: () => {
                            notifications.show({ color: 'green', title: 'Moved to the trash', message: caseLabel(c.number) });
                            void navigate(`/p/${project}/cases`);
                          },
                        })
                      }
                    >
                      Move to the trash
                    </Menu.Item>
                  )}
                </Menu.Dropdown>
              </Menu>
            )}
          </Group>
        }
      />
      <Stack gap="sm">
        {changed && (
          <Alert color="blue" variant="light" icon={<IconInfoCircle size={18} />} title="This case was changed" data-testid="case-changed-banner" className="fp-no-print" withCloseButton onClose={() => setChanged(null)}>
            <Group justify="space-between" gap="xs" wrap="wrap">
              <Text size="sm">
                Revision {changed.revision} was saved {changed.by !== undefined ? `by ${actorLabel(changed.by, me?.name)} ` : ''}through {SOURCE_TEXT[changed.source] ?? changed.source}.{editing ? ' Saving your edits keeps the fields you changed and takes the rest from it.' : ' The page shows it now.'}
              </Text>
              {editing && (
                <Button size="compact-sm" variant="light" leftSection={<IconRefresh size={14} />} onClick={() => { cancel(); void reload(); setChanged(null); }}>
                  Discard my edits and reload
                </Button>
              )}
            </Group>
          </Alert>
        )}
        {stored && !editing && canWrite && (
          <Alert color="yellow" variant="light" title="Unsaved changes from earlier" className="fp-no-print" data-testid="draft-banner">
            <Group justify="space-between" gap="xs" wrap="wrap">
              <Text size="sm">
                Kept in this browser {stored.savedAt ? formatRelative(stored.savedAt) : ''} on revision {stored.revision}
                {stored.revision !== c.revision ? ` (the case is at ${String(c.revision)} now)` : ''}.
              </Text>
              <Group gap="xs">
                <Button size="compact-sm" onClick={() => start(stored.body)} data-testid="draft-restore">
                  Continue editing
                </Button>
                <Button size="compact-sm" variant="default" onClick={() => { clearDraft(project, c.number); setStored(null); }}>
                  Discard
                </Button>
              </Group>
            </Group>
          </Alert>
        )}
        <Suggestions project={project} c={c} etag={etag} items={suggestions.data ?? []} />
        {d && (
          <Group gap="sm" wrap="wrap" className="fp-no-print">
            <Select label="Status" data={CASE_STATUSES.map((s) => ({ value: s, label: CASE_STATUS_LABEL[s] }))} value={d.status ?? c.status} onChange={(v) => v && setDraft({ ...d, status: v })} allowDeselect={false} w={160} comboboxProps={{ withinPortal: true }} data-testid="case-status-select" />
            <Select label="Folder" data={folderData} value={d.folderId ?? null} onChange={(v) => setDraft({ ...d, folderId: v })} searchable clearable w={320} comboboxProps={{ withinPortal: true }} data-testid="case-folder-select" />
          </Group>
        )}
        <Tabs value={tab} onChange={setTab} keepMounted={false}>
          <Tabs.List className="fp-no-print">
            <Tabs.Tab value="scenario" leftSection={<IconListDetails size={14} />}>
              Scenario
            </Tabs.Tab>
            <Tabs.Tab value="details" leftSection={<IconInfoCircle size={14} />}>
              Details
            </Tabs.Tab>
            <Tabs.Tab value="history" leftSection={<IconHistory size={14} />}>
              History
            </Tabs.Tab>
            <Tabs.Tab value="versions" leftSection={<IconVersions size={14} />} rightSection={<Badge size="xs" variant="light">{c.version}</Badge>}>
              Versions
            </Tabs.Tab>
            <Tabs.Tab value="review">Review</Tabs.Tab>
            {FEATURES.requirements && <Tabs.Tab value="requirements">Requirements</Tabs.Tab>}
            {FEATURES.parameters && <Tabs.Tab value="parameters">Parameters</Tabs.Tab>}
          </Tabs.List>
          <Tabs.Panel value="scenario" pt="md">
            {d ? (
              <Stack gap="md">
                <MarkdownField project={project} caseNumber={c.number} label="Description" value={d.body.description} onChange={(v) => setDraft({ ...d, body: { ...d.body, description: v } })} locked={owned.has('description') ? <FromCodeBadge /> : undefined} testId="case-description" minRows={3} inputId="case-description" />
                <MarkdownField project={project} caseNumber={c.number} label="Preconditions" value={d.body.preconditions} onChange={(v) => setDraft({ ...d, body: { ...d.body, preconditions: v } })} testId="case-preconditions" inputId="case-preconditions" />
                <Stack gap={4}>
                  <Text fw={600} size="sm" component="h3" m={0}>
                    Steps
                  </Text>
                  <StepsEditor project={project} caseNumber={c.number} steps={d.body.steps} onChange={(steps) => setDraft({ ...d, body: { ...d.body, steps } })} onAnnounce={setAnnounce} />
                </Stack>
              </Stack>
            ) : (
              <ScenarioView project={project} c={c} />
            )}
          </Tabs.Panel>
          <Tabs.Panel value="details" pt="md">
            <DetailsTab project={project} c={c} fields={fields.data ?? []} draft={d ?? null} onDraft={setDraft} />
          </Tabs.Panel>
          <Tabs.Panel value="history" pt="md">
            <HistoryTab project={project} n={c.number} />
          </Tabs.Panel>
          <Tabs.Panel value="versions" pt="md">
            {editing ? (
              <Text size="sm" c="dimmed">
                Save or cancel your edits to compare and restore versions.
              </Text>
            ) : (
              <VersionsTab project={project} c={c} etag={etag} onRestored={() => setAnnounce('Version restored')} />
            )}
          </Tabs.Panel>
          <Tabs.Panel value="review" pt="md">
            <ReviewTab projectKey={project} caseNumber={c.number} />
          </Tabs.Panel>
          {FEATURES.requirements && (
            <Tabs.Panel value="requirements" pt="md">
              <RequirementsTab projectKey={project} caseNumber={c.number} />
            </Tabs.Panel>
          )}
          {FEATURES.parameters && (
            <Tabs.Panel value="parameters" pt="md">
              <ParametersTab projectKey={project} caseNumber={c.number} />
            </Tabs.Panel>
          )}
        </Tabs>
        <div className="fp-print-only" aria-hidden>
          <ScenarioView project={project} c={c} print />
        </div>
      </Stack>
      {conflict && editing && (
        <ConflictDialog
          conflict={conflict.details}
          current={conflict.current?.value}
          mine={conflict.current ? mergeCaseWrite(editing.base, editing.draft, toWrite(conflict.current.value)) : editing.draft}
          overlap={conflict.current ? bothChanged(editing.base, editing.draft, toWrite(conflict.current.value)) : []}
          me={me?.name}
          labels={labels}
          busy={update.isPending}
          onClose={() => setConflict(null)}
          onTakeTheirs={() => {
            cancel();
            setConflict(null);
            void reload();
          }}
          onKeepMine={() => {
            const cur = conflict.current;
            if (!cur) return;
            const theirs = toWrite(cur.value);
            const merged = mergeCaseWrite(editing.base, editing.draft, theirs);
            setEditing({ draft: merged, base: theirs, etag: cur.etag, revision: cur.value.revision });
            save(merged, cur.etag);
          }}
        />
      )}
    </>
  );
}

export default function CasePage() {
  const { key = '', n = '' } = useParams();
  const project = useProject(key);
  const num = /^\d{1,9}$/.test(n) ? Number(n) : 0;
  const q = useCase(key, num);
  if (!num) return <EmptyState title="Not a case number" description="Case links look like /cases/123." />;
  return (
    <QueryState query={q} notFound="Case not found">
      {() => (q.data ? <CaseBody key={q.data.value.number} project={key} projectName={project.data?.name ?? key} c={q.data.value} etag={q.data.etag} reload={() => q.refetch()} /> : null)}
    </QueryState>
  );
}
