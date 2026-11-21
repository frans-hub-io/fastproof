import { Alert, Badge, Button, Card, Code, Group, Modal, MultiSelect, NumberInput, Radio, Select, Stack, Table, Text, Textarea, TextInput } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconAlertTriangle, IconKey, IconPlus } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { errorMessage } from '../api/client';
import { useCreateToken, useRevokeToken, useTokenOptions, useTokens } from '../api/hooks';
import type { CreatedToken, GrantableRole, TokenInfo, TokenOptions, TokenStatus } from '../api/types';
import { CopyValue } from '../components/CopyButton';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatDateTime, formatRelative } from '../lib/format';
import { buildCreateBody, type ScopeMode } from '../lib/tokens';


const STATUS_COLOR: Record<TokenStatus, string> = { active: 'green', expired: 'gray', revoked: 'red' };

function where(t: TokenInfo): string {
  if (t.global) return 'global';
  if (t.scopes.includes('*')) return 'all projects';
  return t.scopes.join(', ');
}

function grantable(r: GrantableRole): boolean {
  return r.global || r.allScopes || r.scopes.length > 0;
}

function CreateForm({ options, onCreated, onCancel }: { options: TokenOptions; onCreated: (t: CreatedToken) => void; onCancel: () => void }) {
  const roles = options.roles.filter(grantable);
  const [name, setName] = useState('');
  const [role, setRole] = useState<string | null>((roles.find((r) => r.name === 'uploader') ?? roles[0])?.name ?? null);
  const current = roles.find((r) => r.name === role);
  const modes: { value: ScopeMode; label: string; ok: boolean }[] = [
    { value: 'scopes', label: 'Selected projects', ok: !!current && current.scopes.length > 0 },
    { value: 'all', label: 'All projects, including future ones', ok: !!current?.allScopes },
    { value: 'global', label: 'Global (also project creation and settings)', ok: !!current?.global },
  ];
  const [mode, setMode] = useState<ScopeMode>('scopes');
  const effectiveMode = modes.find((m) => m.value === mode)?.ok ? mode : (modes.find((m) => m.ok)?.value ?? 'scopes');
  const [scopes, setScopes] = useState<string[]>([]);
  const [days, setDays] = useState<number | string>(options.defaultDays);
  const [description, setDescription] = useState('');
  const create = useCreateToken();
  const nameOk = /^[A-Za-z0-9._@-]{1,128}$/.test(name.trim());
  const scopeOk = effectiveMode !== 'scopes' || scopes.length > 0;
  const daysOk = typeof days === 'number' && days >= 1 && days <= options.maxDays;

  const submit = () => {
    if (!role) return;
    create.mutate(buildCreateBody({ name, role, mode: effectiveMode, scopes, days, description }), { onSuccess: onCreated });
  };

  return (
    <Stack gap="sm">
      <TextInput
        label="Name"
        description="Letters, digits and . _ @ -; unique among active tokens"
        placeholder="ci-web-shop"
        value={name}
        onChange={(e) => setName(e.currentTarget.value)}
        error={name && !nameOk ? 'Use 1–128 characters: A–Z a–z 0–9 . _ @ -' : undefined}
        data-autofocus
        required
      />
      <Select
        label="Role"
        data={roles.map((r) => ({ value: r.name, label: r.description ? `${r.name} — ${r.description}` : r.name }))}
        value={role}
        onChange={setRole}
        allowDeselect={false}
        required
      />
      {current && (
        <Text size="xs" c="dimmed">
          Permissions: {current.permissions.join(', ')}
        </Text>
      )}
      <Radio.Group label="Where it applies" value={effectiveMode} onChange={(v) => setMode(v)}>
        <Stack gap={6} mt={6}>
          {modes.map((m) => (
            <Radio key={m.value} value={m.value} label={m.label} disabled={!m.ok} />
          ))}
        </Stack>
      </Radio.Group>
      {effectiveMode === 'scopes' && (
        <MultiSelect
          label="Projects"
          placeholder="Pick projects"
          data={current?.scopes ?? []}
          value={scopes}
          onChange={setScopes}
          searchable
          required
        />
      )}
      <NumberInput label="Expires in, days" min={1} max={options.maxDays} value={days} onChange={setDays} description={`1 to ${String(options.maxDays)}`} error={!daysOk ? `Between 1 and ${String(options.maxDays)}` : undefined} />
      <Textarea label="Description" placeholder="What uses this token" value={description} onChange={(e) => setDescription(e.currentTarget.value)} autosize minRows={2} maxLength={1024} />
      {create.isError && (
        <Alert color="red" variant="light" title="The token was not created">
          {errorMessage(create.error)}
        </Alert>
      )}
      <Group justify="flex-end" gap="xs">
        <Button variant="default" onClick={onCancel}>
          Cancel
        </Button>
        <Button onClick={submit} disabled={!nameOk || !role || !scopeOk || !daysOk} loading={create.isPending} data-testid="create-token-submit">
          Create token
        </Button>
      </Group>
    </Stack>
  );
}

function Secret({ token, onClose }: { token: CreatedToken; onClose: () => void }) {
  return (
    <Stack gap="sm" data-testid="token-secret">
      <Alert color="yellow" variant="light" icon={<IconAlertTriangle size={18} />} title="Copy the token now">
        This is the only time FastProof shows it. Store it in your CI secret store; if you lose it, revoke it and create a new one.
      </Alert>
      <Group gap="xs" wrap="nowrap">
        <Code block style={{ flex: 1, wordBreak: 'break-all', whiteSpace: 'pre-wrap' }} data-testid="token-value">
          {token.token}
        </Code>
        <CopyValue value={token.token} label="Copy the token" />
      </Group>
      <Text size="sm" c="dimmed">
        {token.name} · {token.role} · {where(token)} · expires {formatDateTime(token.expiresAt)}
      </Text>
      <Group justify="flex-end">
        <Button onClick={onClose}>I have stored it</Button>
      </Group>
    </Stack>
  );
}

function TokenTable({ tokens, canWrite }: { tokens: TokenInfo[]; canWrite: boolean }) {
  const revoke = useRevokeToken();
  const [confirm, setConfirm] = useState<TokenInfo | null>(null);
  return (
    <>
      <Card withBorder radius="md" padding={0}>
        <div className="fp-table-wrap">
          <Table verticalSpacing="sm" fz="sm" miw={760} data-testid="tokens-table">
            <Table.Thead>
              <Table.Tr>
                <Table.Th>Name</Table.Th>
                <Table.Th>Role</Table.Th>
                <Table.Th>Applies to</Table.Th>
                <Table.Th>Status</Table.Th>
                <Table.Th>Expires</Table.Th>
                <Table.Th>Last used</Table.Th>
                <Table.Th />
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {tokens.map((t) => (
                <Table.Tr key={t.id} data-testid="token-row" data-name={t.name}>
                  <Table.Td>
                    <Stack gap={0}>
                      <Text size="sm" fw={600}>
                        {t.name}
                      </Text>
                      <Text size="xs" c="dimmed">
                        {t.description ?? `by ${t.createdBy}`}
                      </Text>
                    </Stack>
                  </Table.Td>
                  <Table.Td>{t.role}</Table.Td>
                  <Table.Td>{where(t)}</Table.Td>
                  <Table.Td>
                    <Badge size="sm" variant="light" color={STATUS_COLOR[t.status]} style={{ textTransform: 'none' }}>
                      {t.status}
                    </Badge>
                  </Table.Td>
                  <Table.Td title={formatDateTime(t.expiresAt)}>{formatRelative(t.expiresAt)}</Table.Td>
                  <Table.Td>{t.lastUsedAt ? formatRelative(t.lastUsedAt) : 'never'}</Table.Td>
                  <Table.Td>
                    {canWrite && t.status === 'active' && (
                      <Button size="xs" variant="light" color="red" onClick={() => setConfirm(t)} aria-label={`Revoke ${t.name}`}>
                        Revoke
                      </Button>
                    )}
                  </Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </div>
      </Card>
      <Modal opened={!!confirm} onClose={() => setConfirm(null)} title="Revoke the token?">
        {confirm && (
          <Stack gap="sm">
            <Text size="sm">
              <b>{confirm.name}</b> stops working at once on every replica after the cache expires. Tokens it created are revoked with it. This cannot be undone.
            </Text>
            <Group justify="flex-end" gap="xs">
              <Button variant="default" onClick={() => setConfirm(null)}>
                Cancel
              </Button>
              <Button
                color="red"
                loading={revoke.isPending}
                data-testid="confirm-revoke"
                onClick={() =>
                  revoke.mutate(confirm.id, {
                    onSuccess: () => {
                      notifications.show({ color: 'green', message: `${confirm.name} revoked` });
                      setConfirm(null);
                    },
                    onError: (e) => notifications.show({ color: 'red', title: 'Not revoked', message: errorMessage(e) }),
                  })
                }
              >
                Revoke
              </Button>
            </Group>
          </Stack>
        )}
      </Modal>
    </>
  );
}

export default function TokensPage() {
  const canWrite = useCan('tokens:write');
  const tokens = useTokens();
  const options = useTokenOptions();
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<CreatedToken | null>(null);
  const [status, setStatus] = useState<string | null>('active');
  const phone = useMediaQuery('(max-width: 48em)');
  const anyGrantable = useMemo(() => !!options.data?.roles.some(grantable), [options.data]);
  const list = (tokens.data ?? []).filter((t) => !status || t.status === status);

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Settings' }, { label: 'API tokens' }]}
        title="API tokens"
        subtitle="Tokens for CI uploads and scripts. Send them as Authorization: Bearer fp_…"
        actions={
          (canWrite || anyGrantable) && (
            <Button leftSection={<IconPlus size={16} />} onClick={() => setCreating(true)} disabled={!options.data || !anyGrantable} data-testid="create-token">
              Create token
            </Button>
          )
        }
      />
      <Group mb="sm">
        <Select
          aria-label="Token status"
          data={[
            { value: 'active', label: 'Active' },
            { value: 'expired', label: 'Expired' },
            { value: 'revoked', label: 'Revoked' },
          ]}
          value={status}
          onChange={setStatus}
          placeholder="Any status"
          clearable
          w={180}
        />
      </Group>
      <QueryState query={tokens}>
        {() => (list.length ? <TokenTable tokens={list} canWrite={canWrite || anyGrantable} /> : <EmptyState icon={<IconKey size={26} />} title="No tokens" description="Create a token for your CI pipeline to upload results." />)}
      </QueryState>
      <Modal
        opened={creating || !!created}
        onClose={() => {
          if (created) return;
          setCreating(false);
        }}
        closeOnClickOutside={!created}
        closeOnEscape={!created}
        withCloseButton={!created}
        title={created ? 'Token created' : 'Create an API token'}
        size="lg"
        fullScreen={phone}
      >
        {created ? (
          <Secret
            token={created}
            onClose={() => {
              setCreated(null);
              setCreating(false);
            }}
          />
        ) : (
          options.data && <CreateForm options={options.data} onCreated={setCreated} onCancel={() => setCreating(false)} />
        )}
      </Modal>
    </>
  );
}
