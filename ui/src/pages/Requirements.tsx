import { Button, Group, Select, Stack, Switch, Table, Text, TextInput } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { useDebouncedValue } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconCloudDownload, IconPlus, IconSearch } from '@tabler/icons-react';
import { useState } from 'react';
import { useNavigate, useParams, useSearchParams } from 'react-router';
import { useProject } from '../api/hooks';
import { queryErrorOf, REQUIREMENT_STATUSES, requirementError, requirementKey, useRequirementEvents, useRequirements, useRestoreRequirement, type RequirementQuery } from '../api/requirements';
import { PageHeader } from '../components/PageHeader';
import { ImportDialog } from './requirements/ImportDialog';
import { RequirementDialog } from './requirements/RequirementDialog';
import { FlatRows, TreeBranch } from './requirements/RequirementRows';

export default function Requirements() {
  const { key = '' } = useParams();
  const navigate = useNavigate();
  const project = useProject(key);
  const canWrite = useCan('cases:write', key);
  const canImport = useCan('cases:import', key);
  const [params, setParams] = useSearchParams();
  const get = (k: string) => params.get(k) ?? '';
  const set = (k: string, v: string | null) =>
    setParams(
      (p) => {
        const next = new URLSearchParams(p);
        if (v) next.set(k, v);
        else next.delete(k);
        return next;
      },
      { replace: true },
    );
  const [q, setQ] = useState(get('q'));
  const [fpql, setFpql] = useState(get('query'));
  const [debouncedQ] = useDebouncedValue(q, 300);
  const [debouncedFpql] = useDebouncedValue(fpql, 500);
  const archived = get('archived') === 'true';
  const query: RequirementQuery = { q: debouncedQ.trim() || undefined, status: get('status') || undefined, type: get('type').trim() || undefined, query: debouncedFpql.trim() || undefined, archived };
  const flat = !!(query.q || query.status || query.type || query.query || archived);
  const flatList = useRequirements(key, query, flat);
  const queryError = flat ? queryErrorOf(flatList.error) : null;
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const [creating, setCreating] = useState(false);
  const [importing, setImporting] = useState(false);
  const restore = useRestoreRequirement(key);
  useRequirementEvents(key);

  const toggle = (n: number) =>
    setExpanded((s) => {
      const next = new Set(s);
      if (!next.delete(n)) next.add(n);
      return next;
    });
  const doRestore = (n: number) =>
    restore.mutate(n, {
      onSuccess: () => notifications.show({ color: 'green', title: 'Restored', message: `${requirementKey(n)} is back.` }),
      onError: (e) => notifications.show({ color: 'red', title: 'Not restored', message: requirementError(e) }),
    });

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Requirements' }]}
        title="Requirements"
        subtitle="What the product must do. Link cases to a requirement to see how well it is covered."
        actions={
          <Group gap="xs">
            {canImport && (
              <Button size="xs" variant="default" leftSection={<IconCloudDownload size={14} />} onClick={() => setImporting(true)} data-testid="requirements-import">
                Import from tracker
              </Button>
            )}
            {canWrite && (
              <Button size="xs" leftSection={<IconPlus size={14} />} onClick={() => setCreating(true)} data-testid="new-requirement">
                New requirement
              </Button>
            )}
          </Group>
        }
      />
      <Stack gap="sm">
        <Group gap="sm" wrap="wrap" align="flex-end">
          <TextInput
            aria-label="Search requirements"
            placeholder="Search by REQ- number, title or issue key"
            leftSection={<IconSearch size={14} />}
            value={q}
            onChange={(e) => {
              setQ(e.currentTarget.value);
              set('q', e.currentTarget.value.trim() || null);
            }}
            style={{ flex: '1 1 240px' }}
            data-testid="requirements-search"
          />
          <Select aria-label="Status" placeholder="Any status" clearable data={REQUIREMENT_STATUSES.map((s) => ({ value: s, label: s }))} value={get('status') || null} onChange={(v) => set('status', v)} w={{ base: '100%', xs: 150 }} data-testid="requirements-status" />
          <TextInput aria-label="Type" placeholder="Any type" value={get('type')} onChange={(e) => set('type', e.currentTarget.value)} w={{ base: '100%', xs: 140 }} data-testid="requirements-type" />
          <Switch label="Archived" checked={archived} onChange={(e) => set('archived', e.currentTarget.checked ? 'true' : null)} data-testid="requirements-archived" />
        </Group>
        <TextInput
          aria-label="Query"
          placeholder="Query, for example: status = active and coverage = uncovered"
          value={fpql}
          onChange={(e) => {
            setFpql(e.currentTarget.value);
            set('query', e.currentTarget.value.trim() || null);
          }}
          error={queryError ? `${queryError.message}${queryError.position === undefined ? '' : ` (at character ${String(queryError.position + 1)})`}` : undefined}
          className="fp-mono-input"
          data-testid="requirements-query"
        />
        <Table.ScrollContainer minWidth={640}>
          <Table verticalSpacing={6} fz="sm" highlightOnHover data-testid="requirements-table">
            <Table.Thead>
              <Table.Tr>
                <Table.Th>Requirement</Table.Th>
                <Table.Th>Type</Table.Th>
                <Table.Th>Status</Table.Th>
                <Table.Th>Issue</Table.Th>
                <Table.Th ta="right">Cases</Table.Th>
                <Table.Th w={40}>
                  <Text span className="fp-sr-only">
                    Actions
                  </Text>
                </Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {flat ? (
                <FlatRows project={key} query={query} canWrite={canWrite} onRestore={doRestore} empty={archived ? 'Nothing is archived.' : 'No requirement matches.'} />
              ) : (
                <TreeBranch project={key} parent={null} depth={0} expanded={expanded} onToggle={toggle} canWrite={canWrite} />
              )}
            </Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      </Stack>
      {creating && <RequirementDialog project={key} onClose={() => setCreating(false)} onSaved={(r) => void navigate(`/p/${key}/requirements/${String(r.number)}`)} />}
      {importing && <ImportDialog project={key} onClose={() => setImporting(false)} />}
    </>
  );
}
