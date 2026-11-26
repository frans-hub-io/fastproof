import { Alert, Badge, Button, Card, Group, Modal, Stack, Table, Text } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconArchive, IconArrowBackUp, IconPencil, IconPlus } from '@tabler/icons-react';
import { useState } from 'react';
import { useNavigate, useParams } from 'react-router';
import { useProject } from '../api/hooks';
import { coverageOfLinks, requirementError, requirementKey, useArchiveRequirement, useRequirement, useRequirementCases, useRequirementEvents, useRestoreRequirement, useRequirementTree, type Requirement as RequirementDoc } from '../api/requirements';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatDateTime, formatRelative } from '../lib/format';
import { CaseHtml } from './cases/CaseHtml';
import { Section } from './cases/common';
import { CoverageChip, CountsBar } from './requirements/coverage';
import { countsLabel } from './requirements/coverageStyle';
import { IssueLink, RequirementLinkText, RequirementStatusBadge } from './requirements/common';
import { LinkedCases } from './requirements/LinkedCases';
import { RequirementDialog } from './requirements/RequirementDialog';

function Children({ project, number, canWrite, onAdd }: { project: string; number: number; canWrite: boolean; onAdd: () => void }) {
  const q = useRequirementTree(project, number);
  const items = q.data?.pages.flatMap((p) => p.items) ?? [];
  return (
    <Section
      title={`Child requirements${q.data ? ` (${String(items.length)}${q.hasNextPage ? '+' : ''})` : ''}`}
      actions={
        canWrite && (
          <Button size="compact-xs" variant="subtle" leftSection={<IconPlus size={12} />} onClick={onAdd} data-testid="add-child">
            Add child
          </Button>
        )
      }
    >
      {items.length === 0 ? (
        <Text size="sm" c="dimmed" data-testid="children-empty">
          {q.isPending ? 'Loading…' : 'No child requirements.'}
        </Text>
      ) : (
        <Table.ScrollContainer minWidth={420}>
          <Table verticalSpacing={4} fz="sm" data-testid="children-table">
            <Table.Tbody>
              {items.map((c) => (
                <Table.Tr key={c.number}>
                  <Table.Td>
                    <RequirementLinkText project={project} number={c.number} title={c.title} />
                  </Table.Td>
                  <Table.Td>
                    <RequirementStatusBadge status={c.status} />
                  </Table.Td>
                  <Table.Td ta="right">{c.cases} cases</Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      )}
      {q.hasNextPage && (
        <Button variant="subtle" size="xs" onClick={() => void q.fetchNextPage()} loading={q.isFetchingNextPage}>
          Load more
        </Button>
      )}
    </Section>
  );
}

function Summary({ project, number }: { project: string; number: number }) {
  const links = useRequirementCases(project, number);
  const items = links.data?.pages.flatMap((p) => p.items) ?? [];
  if (!links.data) return null;
  const { status, counts } = coverageOfLinks(items);
  return (
    <Card withBorder radius="md" padding="sm" data-testid="coverage-summary">
      <Stack gap={6}>
        <Group gap="xs" justify="space-between">
          <Text size="sm" fw={600}>
            Coverage by the last results
          </Text>
          <CoverageChip status={status} />
        </Group>
        <CountsBar counts={counts} />
        <Text size="xs" c="dimmed">
          {countsLabel(counts)}. Counts this requirement's own active links only; the Coverage page also counts its children.
        </Text>
      </Stack>
    </Card>
  );
}

function Body({ project, projectName, r, etag, reload }: { project: string; projectName: string; r: RequirementDoc; etag: string; reload: () => void }) {
  const canWrite = useCan('cases:write', project);
  const navigate = useNavigate();
  const archive = useArchiveRequirement(project);
  const restore = useRestoreRequirement(project);
  const [editing, setEditing] = useState(false);
  const [adding, setAdding] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const archived = !!r.archivedAt;
  const parentTrail = r.path.slice(0, -1);

  return (
    <>
      <PageHeader
        crumbs={[
          { label: 'Projects', to: '/' },
          { label: projectName, to: `/p/${project}` },
          { label: 'Requirements', to: `/p/${project}/requirements` },
          ...parentTrail.map((p) => ({ label: `${requirementKey(p.number)} ${p.title}`, to: `/p/${project}/requirements/${String(p.number)}` })),
          { label: requirementKey(r.number) },
        ]}
        title={
          <span data-testid="requirement-heading">
            <span className="fp-mono">{requirementKey(r.number)}</span> {r.title}
          </span>
        }
        badges={
          <Group gap={6}>
            <RequirementStatusBadge status={r.status} />
            <Badge size="sm" variant="outline" color="gray" radius="sm" style={{ textTransform: 'none' }}>
              {r.type}
            </Badge>
            {archived && (
              <Badge size="sm" variant="outline" color="gray" radius="sm">
                archived
              </Badge>
            )}
          </Group>
        }
        subtitle={
          <Group gap="sm" wrap="wrap">
            {r.tracker && <IssueLink issueKey={r.tracker.issueKey} url={r.tracker.url} externalState={r.tracker.externalState} />}
            <span title={formatDateTime(r.updatedAt)}>
              Updated {formatRelative(r.updatedAt)}
              {r.updatedBy ? ` by ${r.updatedBy}` : ''}
            </span>
          </Group>
        }
        actions={
          canWrite && (
            <Group gap="xs">
              {!archived && (
                <Button size="xs" leftSection={<IconPencil size={14} />} onClick={() => setEditing(true)} data-testid="requirement-edit">
                  Edit
                </Button>
              )}
              {archived ? (
                <Button
                  size="xs"
                  variant="default"
                  leftSection={<IconArrowBackUp size={14} />}
                  loading={restore.isPending}
                  onClick={() =>
                    restore.mutate(r.number, {
                      onSuccess: () => notifications.show({ color: 'green', message: `${requirementKey(r.number)} is restored.` }),
                      onError: (e) => notifications.show({ color: 'red', title: 'Not restored', message: requirementError(e) }),
                    })
                  }
                  data-testid="requirement-restore"
                >
                  Restore
                </Button>
              ) : (
                <Button size="xs" variant="default" color="red" leftSection={<IconArchive size={14} />} onClick={() => setConfirming(true)} data-testid="requirement-archive">
                  Archive
                </Button>
              )}
            </Group>
          )
        }
      />
      <Stack gap="lg">
        {archived && (
          <Alert color="gray" title="Archived" data-testid="requirement-archived">
            This requirement and everything under it is in the trash. Restore it to link cases again.
          </Alert>
        )}
        <CaseHtmlBlock project={project} html={r.descriptionHtml} />
        <Summary project={project} number={r.number} />
        <Section title="Linked cases">
          <LinkedCases project={project} number={r.number} canWrite={canWrite && !archived} />
        </Section>
        <Children project={project} number={r.number} canWrite={canWrite && !archived} onAdd={() => setAdding(true)} />
      </Stack>
      {editing && (
        <RequirementDialog
          project={project}
          edit={{ requirement: r, etag }}
          onClose={() => setEditing(false)}
          onReload={reload}
          onSaved={() => {
            setEditing(false);
            notifications.show({ color: 'green', message: 'Saved.' });
          }}
        />
      )}
      {adding && <RequirementDialog project={project} parentNumber={r.number} onClose={() => setAdding(false)} onSaved={(c) => void navigate(`/p/${project}/requirements/${String(c.number)}`)} />}
      <Modal opened={confirming} onClose={() => setConfirming(false)} title={`Archive ${requirementKey(r.number)}?`}>
        <Stack gap="sm">
          <Text size="sm">
            {r.children > 0 ? `It and the ${String(r.children)} requirement${r.children === 1 ? '' : 's'} under it move to the trash.` : 'It moves to the trash.'} Links to cases are kept; restore it from the Archived list.
          </Text>
          <Group justify="flex-end">
            <Button variant="default" onClick={() => setConfirming(false)}>
              Cancel
            </Button>
            <Button
              color="red"
              loading={archive.isPending}
              onClick={() =>
                archive.mutate(r.number, {
                  onSuccess: () => void navigate(`/p/${project}/requirements`),
                  onError: (e) => {
                    setConfirming(false);
                    notifications.show({ color: 'red', title: 'Not archived', message: requirementError(e) });
                  },
                })
              }
              data-testid="requirement-archive-confirm"
            >
              Archive
            </Button>
          </Group>
        </Stack>
      </Modal>
    </>
  );
}

function CaseHtmlBlock({ project, html }: { project: string; html: string }) {
  return <CaseHtml html={html} project={project} empty={<Text size="sm" c="dimmed">No description.</Text>} />;
}

export default function RequirementPage() {
  const { key = '', n = '' } = useParams();
  const number = Number(n);
  const project = useProject(key);
  const req = useRequirement(key, number);
  useRequirementEvents(key);
  return (
    <QueryState query={req} notFound="Requirement not found">
      {() => (req.data ? <Body project={key} projectName={project.data?.name ?? key} r={req.data.value} etag={req.data.etag} reload={() => void req.refetch()} /> : null)}
    </QueryState>
  );
}
