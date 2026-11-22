import { Badge, Button, Card, Group, Modal, Stack, Text, TextInput } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { useDebouncedValue } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconLink, IconPlus, IconSearch } from '@tabler/icons-react';
import { useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import { useNavigate } from 'react-router';
import { errorMessage, projectPath, request } from '../../api/client';
import { flatItems } from '../../api/hooks';
import { tmk, useCases, useCaseFolders, useTestIdentities, type Case, type TestIdentity } from '../../api/tm';
import { LoadMore } from '../../components/LoadMore';
import { QueryState } from '../../components/QueryState';
import { formatRelative } from '../../lib/format';
import { caseLabel, parseCaseNumber } from '../../lib/tm/filters';
import { caseLink } from '../../lib/tm/ui';

function LinkDialog({ project, identity, onClose }: { project: string; identity: TestIdentity; onClose: () => void }) {
  const [q, setQ] = useState('');
  const [debounced] = useDebouncedValue(q, 250);
  const n = parseCaseNumber(debounced);
  const cases = useCases(project, { q: debounced || undefined }, debounced.length > 0);
  const items = flatItems(cases.data).slice(0, 20);
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const link = async (number: number) => {
    setBusy(true);
    try {
      await request(`${projectPath(project)}/cases/${String(number)}/identities`, { method: 'POST', json: { testCaseId: identity.testCaseId } });
      void qc.invalidateQueries({ queryKey: tmk.all(project) });
      notifications.show({ color: 'green', title: 'Linked', message: `${identity.displayName} → ${caseLabel(number)}` });
      onClose();
    } catch (e) {
      notifications.show({ color: 'red', title: 'Not linked', message: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal opened onClose={onClose} title={`Link ${identity.displayName} to a case`}>
      <Stack gap="sm">
        <TextInput label="Case" placeholder="TC-number or name" value={q} onChange={(e) => setQ(e.currentTarget.value)} data-autofocus leftSection={<IconSearch size={14} />} data-testid="link-case-search" />
        <Stack gap={4}>
          {items.map((c) => (
            <Button key={c.number} variant="subtle" justify="flex-start" onClick={() => void link(c.number)} loading={busy} data-testid="link-case-option">
              {caseLabel(c.number)} {c.name}
            </Button>
          ))}
          {debounced && !items.length && !cases.isPending && (
            <Text size="sm" c="dimmed">
              {n ? `No case ${caseLabel(n)}.` : 'No case matches.'}
            </Text>
          )}
        </Stack>
      </Stack>
    </Modal>
  );
}

export function UnlinkedTests({ project }: { project: string }) {
  const canWrite = useCan('cases:write', project);
  const [branch, setBranch] = useState('');
  const [q, setQ] = useState('');
  const [debounced] = useDebouncedValue(q, 300);
  const list = useTestIdentities(project, { unlinked: true, branch: branch || undefined, q: debounced || undefined });
  const folders = useCaseFolders(project);
  const navigate = useNavigate();
  const [linking, setLinking] = useState<TestIdentity | null>(null);
  const [creating, setCreating] = useState<string | null>(null);
  const items = flatItems(list.data);

  const create = async (t: TestIdentity) => {
    setCreating(t.testCaseId);
    try {
      const automated = folders.data?.find((f) => f.parentId === null && f.name === 'Automated');
      const c = await request<Case>(`${projectPath(project)}/cases`, { method: 'POST', json: { name: t.displayName, folderId: automated?.id ?? null, status: 'draft', body: { description: '', preconditions: '', steps: [], links: [] } } });
      await request(`${projectPath(project)}/cases/${String(c.number)}/identities`, { method: 'POST', json: { testCaseId: t.testCaseId } });
      void navigate(caseLink(project, c.number));
    } catch (e) {
      notifications.show({ color: 'red', title: 'Case not created', message: errorMessage(e) });
    } finally {
      setCreating(null);
    }
  };

  return (
    <Stack gap="sm" data-testid="unlinked-tests">
      <Text size="sm" c="dimmed">
        Tests seen in launches that no case covers: new tests and tests from branches the code sync does not trust.
      </Text>
      <Group gap="xs" wrap="wrap">
        <TextInput aria-label="Search tests" placeholder="Search by full name" leftSection={<IconSearch size={14} />} value={q} onChange={(e) => setQ(e.currentTarget.value)} style={{ flex: '1 1 240px' }} />
        <TextInput aria-label="Branch" placeholder="Branch, e.g. feature/coupon-v2" value={branch} onChange={(e) => setBranch(e.currentTarget.value.trim())} w={240} />
      </Group>
      <QueryState query={list}>
        {() =>
          items.length === 0 ? (
            <EmptyState title="Every test has a case" description="New tests from trusted branches get a case on the next code sync." />
          ) : (
            <Card withBorder radius="md" padding={0}>
              {items.map((t) => (
                <Group key={t.testCaseId} justify="space-between" wrap="wrap" gap="xs" p="sm" style={{ borderBottom: '1px solid var(--fh-ui-hairline)' }} data-testid="unlinked-row">
                  <Stack gap={2} style={{ minWidth: 0, flex: '1 1 300px' }}>
                    <Text size="sm" fw={600} style={{ wordBreak: 'break-word' }}>
                      {t.displayName}
                    </Text>
                    <Text size="xs" c="dimmed" className="fp-mono" style={{ wordBreak: 'break-all' }}>
                      {t.fullName}
                    </Text>
                    <Group gap={4} wrap="wrap">
                      {t.branches.map((b) => (
                        <Badge key={b} size="xs" variant="outline" color="gray" radius="sm">
                          {b}
                        </Badge>
                      ))}
                      <Text size="xs" c="dimmed">
                        last seen {formatRelative(t.lastSeenAt)}
                      </Text>
                    </Group>
                  </Stack>
                  {canWrite && (
                    <Group gap="xs">
                      <Button size="xs" variant="light" leftSection={<IconPlus size={14} />} loading={creating === t.testCaseId} onClick={() => void create(t)} data-testid="unlinked-create">
                        Create case
                      </Button>
                      <Button size="xs" variant="default" leftSection={<IconLink size={14} />} onClick={() => setLinking(t)} data-testid="unlinked-link">
                        Link to…
                      </Button>
                    </Group>
                  )}
                </Group>
              ))}
              <LoadMore hasMore={list.hasNextPage} loading={list.isFetchingNextPage} onLoad={() => void list.fetchNextPage()} />
            </Card>
          )
        }
      </QueryState>
      {linking && <LinkDialog project={project} identity={linking} onClose={() => setLinking(null)} />}
    </Stack>
  );
}
