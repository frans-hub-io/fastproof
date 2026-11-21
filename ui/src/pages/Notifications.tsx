import { ActionIcon, Anchor, Badge, Button, Card, Checkbox, Group, Modal, Select, SimpleGrid, Stack, Table, TagsInput, Text, TextInput, Title, Tooltip } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconBrandGithub, IconBrandGitlab, IconBrandSlack, IconBrandTelegram, IconEdit, IconMail, IconPlus, IconTrash, IconWebhook } from '@tabler/icons-react';
import { useState } from 'react';
import { Link, useParams } from 'react-router';
import { errorMessage } from '../api/client';
import { useProject } from '../api/hooks';
import { useDeliveries, useSaveSettings, useSecrets, useSettings } from '../api/stageC';
import type { Delivery, NotificationEvent, NotificationKind, NotificationTarget, ProjectSettings } from '../api/types-next';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatDateTime, formatRelative } from '../lib/format';

const KINDS: { value: NotificationKind; label: string; target: string; placeholder: string }[] = [
  { value: 'webhook', label: 'Webhook', target: 'URL', placeholder: 'https://ci.example.com/hooks/fastproof' },
  { value: 'slack', label: 'Slack', target: 'Incoming webhook URL', placeholder: 'https://hooks.slack.com/services/…' },
  { value: 'email', label: 'Email', target: 'Address', placeholder: 'qa-team@example.com' },
  { value: 'telegram', label: 'Telegram', target: 'Chat id or @channel', placeholder: '-1001234567890' },
];

const EVENTS: { value: NotificationEvent; label: string }[] = [
  { value: 'launchFinished', label: 'Launch finished' },
  { value: 'gateFailed', label: 'Gate failed' },
  { value: 'newFailures', label: 'New failures' },
];

const EVENT_LABEL: Record<NotificationEvent, string> = { launchFinished: 'Launch finished', gateFailed: 'Gate failed', newFailures: 'New failures' };

const STATUS_COLOR: Record<Delivery['status'], string> = { delivered: 'green', failed: 'red', sending: 'yellow', pending: 'gray' };

function eventsText(events: string[]): string {
  return events.map((e) => (e in EVENT_LABEL ? EVENT_LABEL[e as NotificationEvent] : e)).join(', ') || '—';
}

/** Icon of a delivery channel: a notification kind, or the VCS provider for `vcs` deliveries. */
function ChannelIcon({ kind, size = 14 }: { kind: string; size?: number }) {
  if (kind === 'gitlab') return <IconBrandGitlab size={size} />;
  if (kind === 'github') return <IconBrandGithub size={size} />;
  return <KindIcon kind={kind as NotificationKind} size={size} />;
}

function KindIcon({ kind, size = 18 }: { kind: NotificationKind; size?: number }) {
  switch (kind) {
    case 'slack':
      return <IconBrandSlack size={size} />;
    case 'email':
      return <IconMail size={size} />;
    case 'telegram':
      return <IconBrandTelegram size={size} />;
    default:
      return <IconWebhook size={size} />;
  }
}

function kindLabel(k: NotificationKind): string {
  return KINDS.find((x) => x.value === k)?.label ?? k;
}

function targetError(kind: NotificationKind, target: string): string | null {
  const t = target.trim();
  if (!t) return 'Required';
  if (t.length > 500) return 'At most 500 characters';
  if (kind === 'webhook' || kind === 'slack') return /^https?:\/\/[^\s/$.?#][^\s]*$/i.test(t) ? null : 'Must be an http(s) URL';
  if (kind === 'email') return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(t) ? null : 'Must be an email address';
  return /^(-?\d{1,20}|@[A-Za-z0-9_]{5,32})$/.test(t) ? null : 'A numeric chat id or @channel';
}

const PATTERN = /^[A-Za-z0-9._/*-]{1,200}$/;

function NotificationEditor({ initial, secrets, onSave, onDelete, onCancel, saving }: { initial: NotificationTarget; secrets: string[]; onSave: (n: NotificationTarget) => void; onDelete?: () => void; onCancel: () => void; saving: boolean }) {
  const [n, setN] = useState<NotificationTarget>(initial);
  const kind = KINDS.find((k) => k.value === n.kind) ?? KINDS[0];
  const urlKind = n.kind === 'webhook' || n.kind === 'slack';
  const fromSecret = urlKind && !!n.targetSecretRef;
  const tError = fromSecret ? null : targetError(n.kind, n.target ?? '');
  const branches = n.filter?.branches ?? [];
  const environments = n.filter?.environments ?? [];
  const filterError = [...branches, ...environments].some((p) => !PATTERN.test(p)) ? 'Use names or globs like release/*' : null;
  const valid = !tError && n.on.length > 0 && !filterError;
  const withCurrent = (cur: string | null | undefined) => (cur && !secrets.includes(cur) ? [...secrets, cur] : secrets);
  const usesSecretRef = n.kind === 'webhook' || n.kind === 'telegram';
  const submit = () => {
    if (!valid) return;
    const filter = branches.length || environments.length ? { branches, environments } : null;
    onSave({
      ...n,
      target: fromSecret ? '' : (n.target ?? '').trim(),
      filter,
      secretRef: usesSecretRef ? (n.secretRef ?? null) : null,
      targetSecretRef: urlKind ? (n.targetSecretRef ?? null) : null,
    });
  };
  return (
    <Stack gap="sm">
      <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="sm">
        <Select label="Kind" data={KINDS.map(({ value, label }) => ({ value, label }))} value={n.kind} onChange={(v) => v && setN({ ...n, kind: v })} allowDeselect={false} comboboxProps={{ withinPortal: true }} data-testid="notification-kind" />
        {usesSecretRef && (
          <Select
            label={n.kind === 'webhook' ? 'Signing secret' : 'Bot token secret'}
            description={n.kind === 'webhook' ? 'Purpose webhook; HMAC-SHA256 key for X-FastProof-Signature' : 'Purpose telegram'}
            placeholder="None"
            data={withCurrent(n.secretRef)}
            value={n.secretRef ?? null}
            onChange={(v) => setN({ ...n, secretRef: v })}
            clearable
            comboboxProps={{ withinPortal: true }}
          />
        )}
      </SimpleGrid>
      {urlKind && (
        <Select
          label="URL from a secret"
          description={n.kind === 'slack' ? 'Recommended: the Slack webhook URL is a credential (purpose slack)' : 'For a URL that carries a token (purpose webhook)'}
          placeholder="None: type the URL below"
          data={withCurrent(n.targetSecretRef)}
          value={n.targetSecretRef ?? null}
          onChange={(v) => setN({ ...n, targetSecretRef: v })}
          clearable
          comboboxProps={{ withinPortal: true }}
          data-testid="notification-target-secret"
        />
      )}
      {!fromSecret && (
        <TextInput label={kind?.target} placeholder={kind?.placeholder} value={n.target ?? ''} onChange={(e) => setN({ ...n, target: e.currentTarget.value })} error={tError} maxLength={500} data-testid="notification-target" data-autofocus />
      )}
      <Checkbox.Group label="Send on" value={n.on} onChange={(v) => setN({ ...n, on: v })} error={n.on.length ? null : 'Pick at least one event'}>
        <Group gap="md" mt={6}>
          {EVENTS.map((e) => (
            <Checkbox key={e.value} value={e.value} label={e.label} />
          ))}
        </Group>
      </Checkbox.Group>
      <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="sm">
        <TagsInput label="Only branches" description="Empty: all" placeholder="main, release/*" value={branches} onChange={(v) => setN({ ...n, filter: { branches: v, environments } })} maxTags={20} error={filterError} />
        <TagsInput label="Only environments" description="Empty: all" placeholder="staging-chromium" value={environments} onChange={(v) => setN({ ...n, filter: { branches, environments: v } })} maxTags={20} />
      </SimpleGrid>
      <Group justify="space-between" mt="xs">
        {onDelete ? (
          <Button variant="subtle" color="red" leftSection={<IconTrash size={16} />} onClick={onDelete} loading={saving}>
            Delete
          </Button>
        ) : (
          <span />
        )}
        <Group gap="xs">
          <Button variant="default" onClick={onCancel}>
            Cancel
          </Button>
          <Button onClick={submit} disabled={!valid} loading={saving} data-testid="notification-save">
            Save
          </Button>
        </Group>
      </Group>
    </Stack>
  );
}

function TargetCard({ n, canWrite, onEdit }: { n: NotificationTarget; canWrite: boolean; onEdit: () => void }) {
  const branches = n.filter?.branches ?? [];
  const environments = n.filter?.environments ?? [];
  return (
    <Card withBorder radius="md" padding="md" data-testid="notification-card">
      <Stack gap={6}>
        <Group justify="space-between" wrap="nowrap" gap="xs">
          <Group gap="xs" wrap="nowrap" style={{ minWidth: 0 }}>
            <KindIcon kind={n.kind} />
            <Text fw={700}>{kindLabel(n.kind)}</Text>
          </Group>
          {canWrite && (
            <Tooltip label="Edit">
              <ActionIcon variant="subtle" aria-label={`Edit the ${kindLabel(n.kind)} notification`} onClick={onEdit}>
                <IconEdit size={16} />
              </ActionIcon>
            </Tooltip>
          )}
        </Group>
        <Text size="sm" className="fp-mono" style={{ wordBreak: 'break-all' }} c={n.target ? undefined : 'dimmed'}>
          {n.targetSecretRef ? `URL in secret ${n.targetSecretRef}` : n.target || 'hidden (needs settings:write)'}
        </Text>
        <Group gap={6} wrap="wrap">
          {n.on.map((e) => (
            <Badge key={e} size="sm" variant="light" color={e === 'gateFailed' ? 'red' : e === 'newFailures' ? 'orange' : 'fhPurple'} radius="sm" style={{ textTransform: 'none' }}>
              {EVENT_LABEL[e]}
            </Badge>
          ))}
        </Group>
        <Text size="xs" c="dimmed">
          {n.secretRef ? `secret ${n.secretRef}` : 'no secret'} · branches: {branches.length ? branches.join(', ') : 'all'} · environments: {environments.length ? environments.join(', ') : 'all'}
        </Text>
      </Stack>
    </Card>
  );
}

function Deliveries({ project, settings }: { project: string; settings: ProjectSettings }) {
  const q = useDeliveries(project);
  const byId = new Map(settings.notifications.map((n) => [n.id, n] as const));
  return (
    <Stack gap="sm">
      <Group justify="space-between" wrap="wrap">
        <Title order={4}>Deliveries</Title>
        <Text size="xs" c="dimmed">
          Failed deliveries are retried with backoff from the outbox.
        </Text>
      </Group>
      <QueryState query={q}>
        {() =>
          q.data?.length ? (
            <Card withBorder radius="md" padding={0}>
              <div className="fp-table-wrap">
                <Table verticalSpacing={6} horizontalSpacing="sm" fz="sm" miw={860} data-testid="deliveries-table">
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th w={100}>Status</Table.Th>
                      <Table.Th>When</Table.Th>
                      <Table.Th>Event</Table.Th>
                      <Table.Th>Channel</Table.Th>
                      <Table.Th>Attempts</Table.Th>
                      <Table.Th>Response</Table.Th>
                      <Table.Th>Last error</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {q.data.map((d) => (
                      <Table.Tr key={d.id} data-testid="delivery-row">
                        <Table.Td>
                          <Badge size="sm" variant="light" color={STATUS_COLOR[d.status]} radius="sm" styles={{ root: { textTransform: 'none', flex: 'none', minWidth: 'max-content' }, label: { overflow: 'visible' } }}>
                            {d.status}
                          </Badge>
                        </Table.Td>
                        <Table.Td c="dimmed" title={formatDateTime(d.createdAt)} style={{ whiteSpace: 'nowrap' }}>
                          {formatRelative(d.createdAt)}
                        </Table.Td>
                        <Table.Td style={{ whiteSpace: 'nowrap' }}>
                          {d.launchId ? (
                            <Anchor component={Link} to={`/p/${project}/launches/${d.launchId}`} size="sm" underline="hover">
                              {eventsText(d.events)}
                            </Anchor>
                          ) : (
                            eventsText(d.events)
                          )}
                        </Table.Td>
                        <Table.Td style={{ maxWidth: 260 }}>
                          <Group gap={6} wrap="nowrap">
                            <ChannelIcon kind={d.kind} />
                            <Text size="sm" className="fh-truncate fp-mono" title={byId.get(d.notificationId)?.target || d.notificationId}>
                              {d.notificationId === 'vcs' ? `${d.kind} (VCS)` : d.notificationId}
                            </Text>
                          </Group>
                        </Table.Td>
                        <Table.Td title={d.nextAttemptAt ? `Next attempt ${formatDateTime(d.nextAttemptAt)}` : undefined}>
                          {d.attempts}/{d.maxAttempts}
                        </Table.Td>
                        <Table.Td className="fp-mono">{d.responseStatus ?? '—'}</Table.Td>
                        <Table.Td c={d.lastError ? 'var(--mantine-color-error)' : 'dimmed'} style={{ maxWidth: 280 }}>
                          <Text size="sm" className="fh-truncate" title={d.lastError ?? ''} c="inherit">
                            {d.lastError ?? '—'}
                          </Text>
                        </Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </div>
            </Card>
          ) : (
            <EmptyState title="No deliveries yet" description="Deliveries appear here when a launch triggers a notification." />
          )
        }
      </QueryState>
    </Stack>
  );
}

function newId(): string {
  return `n-${Date.now().toString(36)}${Math.floor(Math.random() * 1e6).toString(36)}`;
}

export default function NotificationsPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const canWrite = useCan('settings:write', key);
  const q = useSettings(key);
  const save = useSaveSettings(key);
  const secrets = useSecrets(key, canWrite);
  const phone = useMediaQuery('(max-width: 48em)');
  const [editing, setEditing] = useState<{ index: number | null; n: NotificationTarget } | null>(null);

  const persist = (settings: ProjectSettings, list: NotificationTarget[], message: string) =>
    save.mutate(
      { ...settings, notifications: list },
      {
        onSuccess: () => {
          notifications.show({ color: 'green', message });
          setEditing(null);
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Notifications not saved', message: errorMessage(e) }),
      },
    );

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Notifications' }]}
        title="Notifications"
        subtitle="Webhooks, Slack, email and Telegram messages on launch events. Webhook bodies are signed with HMAC-SHA256 (X-FastProof-Signature)."
        actions={
          canWrite && q.data ? (
            <Button leftSection={<IconPlus size={16} />} onClick={() => setEditing({ index: null, n: { id: newId(), kind: 'webhook', on: ['launchFinished'], target: '', secretRef: null, filter: null } })} data-testid="notification-add">
              Add notification
            </Button>
          ) : undefined
        }
      />
      <QueryState query={q}>
        {() => {
          const settings = q.data;
          if (!settings) return null;
          return (
            <Stack gap="lg">
              {settings.notifications.length ? (
                <SimpleGrid cols={{ base: 1, md: 2, xl: 3 }} spacing="md" data-testid="notifications-list">
                  {settings.notifications.map((n, i) => (
                    <TargetCard key={n.id} n={n} canWrite={canWrite} onEdit={() => setEditing({ index: i, n: structuredClone(n) })} />
                  ))}
                </SimpleGrid>
              ) : (
                <EmptyState title="No notifications" description={canWrite ? 'Add a target to hear about finished launches, failed gates and new failures.' : 'Project admins can add notifications.'} />
              )}
              <Deliveries project={key} settings={settings} />
              <Modal opened={!!editing} onClose={() => setEditing(null)} title={editing?.index === null ? 'New notification' : 'Edit notification'} size="lg" fullScreen={phone}>
                {editing && (
                  <NotificationEditor
                    initial={editing.n}
                    secrets={(secrets.data ?? []).map((s) => s.name)}
                    saving={save.isPending}
                    onCancel={() => setEditing(null)}
                    onDelete={editing.index === null ? undefined : () => persist(settings, settings.notifications.filter((_, j) => j !== editing.index), 'Notification deleted')}
                    onSave={(n) => persist(settings, editing.index === null ? [...settings.notifications, n] : settings.notifications.map((x, j) => (j === editing.index ? n : x)), 'Notification saved')}
                  />
                )}
              </Modal>
            </Stack>
          );
        }}
      </QueryState>
    </>
  );
}
