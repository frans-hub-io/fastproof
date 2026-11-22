import { ActionIcon, Anchor, Badge, Button, Card, Group, Modal, MultiSelect, NumberInput, Select, SimpleGrid, Stack, TagsInput, Text, TextInput, Tooltip } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconExternalLink, IconPlus, IconTrash, IconUnlink } from '@tabler/icons-react';
import { useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { useLinkIdentity, type Case, type CaseFieldDef, type CaseLink, type CaseWrite, type FieldValue } from '../../api/tm';
import { formatRelative, isHttpUrl } from '../../lib/format';
import { fieldText } from '../../lib/tm/versionDiff';
import { FromCodeBadge, Section } from './common';

function FieldInput({ def, value, onChange, locked }: { def: CaseFieldDef; value: FieldValue | undefined; onChange: (v: FieldValue) => void; locked: boolean }) {
  const label = (
    <Group gap={6} component="span" wrap="nowrap">
      <span>{def.name}</span>
      {locked && <FromCodeBadge hint={`Set by the code sync from the test's ${def.sourceLabel ?? def.key} label`} />}
    </Group>
  );
  const opts = def.options.filter((o) => !o.archived || o.value === value).map((o) => o.value);
  const common = { label, disabled: locked, required: def.required, 'data-testid': `field-${def.key}` };
  switch (def.type) {
    case 'select':
      return <Select {...common} data={opts} value={typeof value === 'string' ? value : null} onChange={(v) => onChange(v)} clearable={!def.required} comboboxProps={{ withinPortal: true }} />;
    case 'multiselect':
      return <MultiSelect {...common} data={opts} value={Array.isArray(value) ? value : []} onChange={onChange} comboboxProps={{ withinPortal: true }} />;
    case 'number':
      return <NumberInput {...common} value={typeof value === 'number' ? value : ''} onChange={(v) => onChange(typeof v === 'number' ? v : null)} />;
    case 'date':
      return <TextInput {...common} type="date" value={typeof value === 'string' ? value : ''} onChange={(e) => onChange(e.currentTarget.value || null)} />;
    default:
      return <TextInput {...common} value={typeof value === 'string' ? value : ''} onChange={(e) => onChange(e.currentTarget.value || null)} maxLength={500} />;
  }
}

function LinksEditor({ links, onChange, locked }: { links: CaseLink[]; onChange: (l: CaseLink[]) => void; locked: boolean }) {
  const set = (i: number, patch: Partial<CaseLink>) => onChange(links.map((l, j) => (j === i ? { ...l, ...patch } : l)));
  return (
    <Stack gap={6}>
      {links.map((l, i) => (
        <Group key={i} gap={6} wrap="wrap" align="flex-end">
          <Select aria-label="Link type" data={['link', 'issue', 'tms']} value={l.type} onChange={(v) => set(i, { type: v === 'issue' || v === 'tms' ? v : 'link' })} w={90} disabled={locked} comboboxProps={{ withinPortal: true }} />
          <TextInput aria-label="Link name" placeholder="Name" value={l.name} onChange={(e) => set(i, { name: e.currentTarget.value })} style={{ flex: '1 1 140px' }} disabled={locked} />
          <TextInput aria-label="Link URL" placeholder="https://…" value={l.url} onChange={(e) => set(i, { url: e.currentTarget.value })} style={{ flex: '2 1 200px' }} disabled={locked} error={l.url && !isHttpUrl(l.url) ? 'http(s) only' : undefined} />
          {!locked && (
            <ActionIcon variant="subtle" color="red" aria-label="Remove link" onClick={() => onChange(links.filter((_, j) => j !== i))}>
              <IconTrash size={14} />
            </ActionIcon>
          )}
        </Group>
      ))}
      {!locked && (
        <Button size="compact-xs" variant="subtle" leftSection={<IconPlus size={12} />} onClick={() => onChange([...links, { type: 'link', name: '', url: '' }])} w="fit-content">
          Add link
        </Button>
      )}
    </Stack>
  );
}

export function DetailsTab({ project, c, fields, draft, onDraft }: { project: string; c: Case; fields: CaseFieldDef[]; draft: CaseWrite | null; onDraft: (d: CaseWrite) => void }) {
  const canWrite = useCan('cases:write', project);
  const link = useLinkIdentity(project, c.number);
  const owned = new Set(c.codeOwned);
  const values = draft?.fields ?? c.fields;
  const tags = draft?.tags ?? c.tags;
  const links = draft?.body.links ?? c.body.links;
  const active = fields.filter((f) => !f.archived);

  const [unlinking, setUnlinking] = useState<{ testCaseId: string; fullName: string } | null>(null);
  const unlink = (testCaseId: string, fullName: string) => setUnlinking({ testCaseId, fullName });

  return (
    <Stack gap="lg" data-testid="details-tab">
      <Section title="Fields">
        {draft ? (
          <SimpleGrid cols={{ base: 1, sm: 2, lg: 3 }} spacing="sm">
            {active.map((def) => (
              <FieldInput key={def.key} def={def} value={values[def.key]} locked={owned.has(def.key)} onChange={(v) => onDraft({ ...draft, fields: { ...draft.fields, [def.key]: v } })} />
            ))}
          </SimpleGrid>
        ) : (
          <SimpleGrid cols={{ base: 1, sm: 2, lg: 3 }} spacing="xs">
            {active.map((def) => (
              <Stack key={def.key} gap={0} data-testid={`field-view-${def.key}`}>
                <Group gap={6} wrap="nowrap">
                  <Text size="xs" c="dimmed">
                    {def.name}
                  </Text>
                  {owned.has(def.key) && <FromCodeBadge hint={`Set by the code sync from the test's ${def.sourceLabel ?? def.key} label`} />}
                </Group>
                <Text size="sm">{fieldText(values[def.key]) || '—'}</Text>
              </Stack>
            ))}
          </SimpleGrid>
        )}
      </Section>
      <Section title="Tags" actions={owned.has('tags') ? <FromCodeBadge /> : undefined}>
        {draft && !owned.has('tags') ? (
          <TagsInput aria-label="Tags" value={tags} onChange={(v) => onDraft({ ...draft, tags: v.slice(0, 50) })} data-testid="tags-input" />
        ) : (
          <Group gap={4} wrap="wrap">
            {tags.length ? tags.map((t) => <Badge key={t} variant="light" color="gray" radius="sm">{t}</Badge>) : <Text size="sm" c="dimmed">No tags</Text>}
          </Group>
        )}
      </Section>
      <Section title="Links" actions={owned.has('links') ? <FromCodeBadge /> : undefined}>
        {draft ? (
          <LinksEditor links={links} locked={owned.has('links')} onChange={(l) => onDraft({ ...draft, body: { ...draft.body, links: l } })} />
        ) : links.length ? (
          <Stack gap={2}>
            {links.map((l, i) =>
              isHttpUrl(l.url) ? (
                <Anchor key={i} href={l.url} target="_blank" rel="noopener noreferrer" size="sm">
                  {l.type}: {l.name || l.url} <IconExternalLink size={12} />
                </Anchor>
              ) : (
                <Text key={i} size="sm">
                  {l.type}: {l.name || l.url}
                </Text>
              ),
            )}
          </Stack>
        ) : (
          <Text size="sm" c="dimmed">
            No links
          </Text>
        )}
      </Section>
      <Section title="Tests in code">
        {c.identities.some((i) => i.kind === 'code') ? (
          <Stack gap="xs">
            {c.identities.filter((i) => i.kind === 'code').map((i) => (
              <Card key={i.testCaseId} withBorder radius="md" padding="xs" data-testid="identity-row">
                <Group justify="space-between" wrap="wrap" gap="xs">
                  <Stack gap={2} style={{ minWidth: 0, flex: '1 1 260px' }}>
                    <Anchor component={Link} to={`/p/${project}/test-cases/${i.testCaseId}`} size="sm" className="fp-mono" style={{ wordBreak: 'break-all' }}>
                      {i.fullName}
                    </Anchor>
                    <Group gap={4} wrap="wrap">
                      <Badge size="xs" variant="light" radius="sm">
                        {i.kind}
                      </Badge>
                      {i.branches.map((b) => (
                        <Badge key={b} size="xs" variant="outline" color="gray" radius="sm">
                          {b}
                        </Badge>
                      ))}
                      <Text size="xs" c="dimmed">
                        last seen {formatRelative(i.lastSeenAt)}
                      </Text>
                    </Group>
                  </Stack>
                  {canWrite && (
                    <Tooltip label="Unlink">
                      <ActionIcon variant="subtle" color="red" aria-label={`Unlink ${i.fullName}`} onClick={() => unlink(i.testCaseId, i.fullName)}>
                        <IconUnlink size={16} />
                      </ActionIcon>
                    </Tooltip>
                  )}
                </Group>
              </Card>
            ))}
            {c.identitiesTotal > c.identities.length && (
              <Text size="xs" c="dimmed" data-testid="identities-more">
                Showing {c.identities.length} of {c.identitiesTotal} tests.
              </Text>
            )}
          </Stack>
        ) : (
          <Text size="sm" c="dimmed">
            A manual case. Link a test from{' '}
            <Anchor component={Link} to={`/p/${project}/cases?tab=unlinked`} size="sm" underline="always">
              Unlinked tests
            </Anchor>{' '}
            or put <code>@allure.id={c.keys.find((k) => k.kind === 'allure_id')?.value ?? String(c.number)}</code> on a test to automate it.
          </Text>
        )}
      </Section>
      <Section title="External keys">
        {c.keys.length ? (
          <Group gap={4} wrap="wrap">
            {c.keys.map((k) => (
              <Badge key={`${k.kind}:${k.value}`} variant="outline" color="gray" radius="sm">
                {k.kind}: {k.value}
              </Badge>
            ))}
          </Group>
        ) : (
          <Text size="sm" c="dimmed">
            None
          </Text>
        )}
      </Section>
      <Modal opened={!!unlinking} onClose={() => setUnlinking(null)} title="Unlink the test">
        <Stack gap="sm">
          <Text size="sm">Results of {unlinking?.fullName} stop counting for this case. The test appears under Unlinked tests.</Text>
          <Group justify="flex-end">
            <Button variant="default" onClick={() => setUnlinking(null)}>
              Cancel
            </Button>
            <Button
              color="red"
              loading={link.isPending}
              onClick={() =>
                unlinking &&
                link.mutate(
                  { testCaseId: unlinking.testCaseId, unlink: true },
                  { onSuccess: () => setUnlinking(null), onError: (e) => notifications.show({ color: 'red', title: 'Not unlinked', message: errorMessage(e) }) },
                )
              }
            >
              Unlink
            </Button>
          </Group>
        </Stack>
      </Modal>
    </Stack>
  );
}
