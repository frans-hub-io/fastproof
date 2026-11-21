import { Badge, Card, Code, SimpleGrid, Stack, Table, Text, TextInput } from '@mantine/core';
import { EmptyState, ErrorState, useCan, useMe } from '@franshub/ui-shell';
import { useDebouncedValue } from '@mantine/hooks';
import { IconShieldLock } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { useAudit } from '../api/hooks';
import type { AuditEvent, AuditFilters } from '../api/types';
import { LoadMore } from '../components/LoadMore';
import { PageHeader } from '../components/PageHeader';
import { Loading, QueryState } from '../components/QueryState';
import { formatDateTime, formatRelative } from '../lib/format';

function detailText(d: unknown): string {
  if (d === null || d === undefined) return '';
  if (typeof d === 'string') return d;
  try {
    return JSON.stringify(d);
  } catch {
    return '(unreadable)';
  }
}

function Row({ e }: { e: AuditEvent }) {
  return (
    <Table.Tr data-testid="audit-row">
      <Table.Td style={{ whiteSpace: 'nowrap' }} title={formatDateTime(e.at)}>
        {formatRelative(e.at)}
      </Table.Td>
      <Table.Td>
        <Stack gap={0}>
          <Text size="sm">{e.principal}</Text>
          <Text size="xs" c="dimmed">
            {[e.principalKind, e.roles].filter(Boolean).join(' · ')}
          </Text>
        </Stack>
      </Table.Td>
      <Table.Td>
        <Badge variant="light" color="fhPurple" radius="sm" style={{ textTransform: 'none' }} className="fp-mono">
          {e.action}
        </Badge>
      </Table.Td>
      <Table.Td>{e.scope ?? <Text size="xs" c="dimmed">global</Text>}</Table.Td>
      <Table.Td>
        <Code style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-all' }}>{detailText(e.detail)}</Code>
      </Table.Td>
    </Table.Tr>
  );
}

export default function AuditPage() {
  const { loading } = useMe();
  const allowed = useCan('audit:read');
  const [action, setAction] = useState('');
  const [principal, setPrincipal] = useState('');
  const [scope, setScope] = useState('');
  const [d] = useDebouncedValue({ action, principal, scope }, 300);
  const filters: AuditFilters = useMemo(() => ({ action: d.action || undefined, principal: d.principal || undefined, scope: d.scope || undefined }), [d]);
  const q = useAudit(filters, allowed);
  const items = q.data?.pages.flatMap((p) => p.items) ?? [];

  if (loading) return <Loading />;
  if (!allowed) {
    return (
      <>
        <PageHeader crumbs={[{ label: 'Settings' }, { label: 'Audit log' }]} title="Audit log" />
        <ErrorState title="Administrators only" message="The audit log needs the audit:read permission. Ask an administrator if you need it." />
      </>
    );
  }
  return (
    <>
      <PageHeader crumbs={[{ label: 'Settings' }, { label: 'Audit log' }]} title="Audit log" subtitle="Every change and sign-in, newest first." />
      <Card withBorder padding="sm" radius="md" mb="md">
        <SimpleGrid cols={{ base: 1, sm: 3 }} spacing="xs">
          <TextInput aria-label="Action" placeholder="Action, e.g. token.* or launch.delete" value={action} onChange={(e) => setAction(e.currentTarget.value)} />
          <TextInput aria-label="Principal" placeholder="Principal" value={principal} onChange={(e) => setPrincipal(e.currentTarget.value)} />
          <TextInput aria-label="Project" placeholder="Project key" value={scope} onChange={(e) => setScope(e.currentTarget.value)} />
        </SimpleGrid>
      </Card>
      <QueryState query={q}>
        {() =>
          items.length === 0 ? (
            <EmptyState icon={<IconShieldLock size={26} />} title="No audit records" description="Nothing matches these filters." />
          ) : (
            <Card withBorder radius="md" padding={0}>
              <div className="fp-table-wrap">
                <Table verticalSpacing="xs" fz="sm" miw={760} data-testid="audit-table">
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th>When</Table.Th>
                      <Table.Th>Who</Table.Th>
                      <Table.Th>Action</Table.Th>
                      <Table.Th>Project</Table.Th>
                      <Table.Th>Details</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {items.map((e) => (
                      <Row key={e.id} e={e} />
                    ))}
                  </Table.Tbody>
                </Table>
              </div>
              <LoadMore hasMore={q.hasNextPage} loading={q.isFetchingNextPage} onLoad={() => void q.fetchNextPage()} />
            </Card>
          )
        }
      </QueryState>
    </>
  );
}
