import { ActionIcon, Alert, Badge, Button, Card, ColorInput, Fieldset, Group, Modal, NumberInput, PasswordInput, Select, SimpleGrid, Stack, Switch, Table, Tabs, TagsInput, Text, TextInput, Title, Tooltip } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconEdit, IconForms, IconLock, IconPlus, IconTrash } from '@tabler/icons-react';
import { useMemo, useState, type ReactNode } from 'react';
import { Link, useParams, useSearchParams } from 'react-router';
import { errorMessage } from '../api/client';
import { useProject } from '../api/hooks';
import { useDefectTypes, useDeleteDefectType, useSaveDefectType, useSaveSettings, useSecrets, useSettings, useWriteSecret } from '../api/stageC';
import type { DefectGroup, DefectType, IssueTracker, IssueTrackerType, ProjectSettings, RerunProvider, RerunSettings, SecretPurpose, VcsSettings } from '../api/types-next';
import { DEFECT_GROUPS } from '../api/types-next';
import { CopyValue } from '../components/CopyButton';
import { DefectChip } from '../components/Defect';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { GROUP_COLOR, GROUP_LABEL, issueUrlFor } from '../lib/defects';
import { formatDateTime, formatRelative } from '../lib/format';

const HTTP_URL = /^https?:\/\/[^\s/$.?#][^\s]*$/i;
const SECRET_NAME = /^[a-z0-9][a-z0-9._-]{0,62}$/;

function toInt(v: number | string): number | null {
  const n = typeof v === 'number' ? v : Number(v);
  return v === '' || !Number.isFinite(n) ? null : Math.trunc(n);
}

function rangeError(v: number | null, min: number, max: number, required = true): string | null {
  if (v === null) return required ? 'Required' : null;
  return v < min || v > max ? `Must be ${String(min)}–${String(max)}` : null;
}

function useSave(key: string) {
  const save = useSaveSettings(key);
  const run = (body: ProjectSettings, what: string, after?: () => void) =>
    save.mutate(body, {
      onSuccess: () => {
        notifications.show({ color: 'green', message: `${what} saved` });
        after?.();
      },
      onError: (e) => notifications.show({ color: 'red', title: `${what} not saved`, message: errorMessage(e) }),
    });
  return { run, pending: save.isPending };
}

function Section({ title, description, children }: { title: string; description?: ReactNode; children: ReactNode }) {
  return (
    <Card withBorder radius="md" padding="md">
      <Stack gap="sm">
        <div>
          <Title order={5}>{title}</Title>
          {description && (
            <Text size="sm" c="dimmed">
              {description}
            </Text>
          )}
        </div>
        {children}
      </Stack>
    </Card>
  );
}

function GeneralForm({ project, settings, canWrite }: { project: string; settings: ProjectSettings; canWrite: boolean }) {
  const [depth, setDepth] = useState<number | string>(settings.flaky.historyDepth);
  const [stab, setStab] = useState<number | string>(settings.flaky.stabilizationPeriod);
  const [includePassed, setIncludePassed] = useState(settings.flaky.includePassedTests);
  const [rawDays, setRawDays] = useState<number | string>(settings.retention.rawDays);
  const [keep, setKeep] = useState<number | string>(settings.retention.launchesKeep ?? '');
  const [keepImportant, setKeepImportant] = useState(settings.retention.keepImportant);
  const [versionsKeep, setVersionsKeep] = useState<number | string>(settings.retention.caseVersionsKeep || '');
  const [versionsDays, setVersionsDays] = useState<number | string>(settings.retention.caseVersionsDays || '');
  const [rollup, setRollup] = useState<number | string>(settings.rollup.afterDays);
  const { run, pending } = useSave(project);

  const d = toInt(depth);
  const errors = {
    depth: rangeError(d, 2, 100),
    stab: rangeError(toInt(stab), 0, d ?? 100),
    rawDays: rangeError(toInt(rawDays), 1, 3650),
    keep: rangeError(toInt(keep), 1, 1_000_000, false),
    versionsKeep: rangeError(toInt(versionsKeep), 1, 100_000, false),
    versionsDays: rangeError(toInt(versionsDays), 1, 36_500, false),
    rollup: rangeError(toInt(rollup), 1, 3650),
  };
  const valid = Object.values(errors).every((e) => e === null);

  const submit = () => {
    if (!valid) return;
    const launchesKeep = toInt(keep);
    run(
      {
        ...settings,
        flaky: { historyDepth: d ?? 10, stabilizationPeriod: toInt(stab) ?? 0, includePassedTests: includePassed },
        retention: {
          ...settings.retention,
          rawDays: toInt(rawDays) ?? 90,
          keepImportant,
          launchesKeep: launchesKeep ?? undefined,
          caseVersionsKeep: toInt(versionsKeep) ?? 0,
          caseVersionsDays: toInt(versionsDays) ?? 0,
        },
        rollup: { afterDays: toInt(rollup) ?? 30 },
      },
      'General settings',
    );
  };

  return (
    <Fieldset variant="unstyled" disabled={!canWrite}>
      <Stack gap="md">
        <Section title="Flaky tests" description="A test is flaky when its status flips within the last runs of its series (passed retries count too).">
          <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="sm">
            <NumberInput label="History depth" description="Runs of a series to look at" min={2} max={100} value={depth} onChange={setDepth} error={errors.depth} data-testid="flaky-history-depth" />
            <NumberInput label="Stabilization period" description="Consecutive stable runs before a test stops being flaky" min={0} max={100} value={stab} onChange={setStab} error={errors.stab} />
          </SimpleGrid>
          <Switch label="Count passed tests with retries as flaky" checked={includePassed} onChange={(e) => setIncludePassed(e.currentTarget.checked)} />
        </Section>
        <Section title="Retention" description="Raw results, steps, logs and attachments are deleted after this; daily rollups keep the history for dashboards.">
          <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="sm">
            <NumberInput label="Keep raw data (days)" description="Then only rollups remain" min={1} max={3650} value={rawDays} onChange={setRawDays} error={errors.rawDays} />
            <NumberInput label="Keep at most (launches)" description="Empty: no limit" min={1} max={1_000_000} value={keep} onChange={setKeep} error={errors.keep} />
          </SimpleGrid>
          <Switch label="Never delete launches marked important" checked={keepImportant} onChange={(e) => setKeepImportant(e.currentTarget.checked)} />
          <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="sm">
            <NumberInput label="Keep test case versions (newest per case)" description="Empty: no count; versions run items used are always kept" min={1} max={100_000} value={versionsKeep} onChange={setVersionsKeep} error={errors.versionsKeep} data-testid="settings-case-versions-keep" />
            <NumberInput label="Prune test case versions older than (days)" description="Empty: no age; with both set, a version must be beyond the count and older" min={1} max={36_500} value={versionsDays} onChange={setVersionsDays} error={errors.versionsDays} data-testid="settings-case-versions-days" />
          </SimpleGrid>
        </Section>
        <Section title="Rollup" description="Results older than this are summarized into daily series.">
          <NumberInput label="Roll up after (days)" min={1} max={3650} value={rollup} onChange={setRollup} error={errors.rollup} maw={320} />
        </Section>
        {canWrite && (
          <Group>
            <Button onClick={submit} loading={pending} disabled={!valid} data-testid="settings-save">
              Save
            </Button>
          </Group>
        )}
      </Stack>
    </Fieldset>
  );
}

interface TypeDraft {
  id?: string;
  builtIn: boolean;
  group: DefectGroup;
  name: string;
  shortName: string;
  color: string;
}

function DefectTypeEditor({ project, draft, onClose }: { project: string; draft: TypeDraft; onClose: () => void }) {
  const [d, setD] = useState(draft);
  const save = useSaveDefectType(project);
  const nameError = !d.name.trim() ? 'Required' : d.name.length > 80 ? 'At most 80 characters' : null;
  const shortError = !d.shortName.trim() ? 'Required' : d.shortName.length > 6 ? 'At most 6 characters' : null;
  const colorError = /^#[0-9a-fA-F]{6}$/.test(d.color) ? null : 'Use #rrggbb';
  const valid = !nameError && !shortError && !colorError;
  const submit = () => {
    if (!valid) return;
    save.mutate(
      { id: d.id, body: { group: d.group, name: d.name.trim(), shortName: d.shortName.trim().toUpperCase(), color: d.color } },
      {
        onSuccess: () => {
          notifications.show({ color: 'green', message: `${d.name} saved` });
          onClose();
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Not saved', message: errorMessage(e) }),
      },
    );
  };
  return (
    <Stack gap="sm">
      <Select
        label="Group"
        description={d.builtIn ? 'Built-in types keep their group' : 'Dashboards and gates count defects by group'}
        data={DEFECT_GROUPS.map((g) => ({ value: g, label: GROUP_LABEL[g] }))}
        value={d.group}
        onChange={(v) => v && setD({ ...d, group: v })}
        disabled={d.builtIn}
        allowDeselect={false}
        comboboxProps={{ withinPortal: true }}
      />
      <TextInput label="Name" value={d.name} onChange={(e) => setD({ ...d, name: e.currentTarget.value })} error={d.name ? nameError : null} maxLength={80} data-autofocus />
      <SimpleGrid cols={2} spacing="sm">
        <TextInput label="Short name" value={d.shortName} onChange={(e) => setD({ ...d, shortName: e.currentTarget.value.toUpperCase() })} error={d.shortName ? shortError : null} maxLength={6} />
        <ColorInput label="Color" format="hex" value={d.color} onChange={(c) => setD({ ...d, color: c })} error={colorError} swatches={[...Object.values(GROUP_COLOR), '#fab005', '#4dabf7', '#40c057', '#f06595']} swatchesPerRow={9} popoverProps={{ withinPortal: true }} />
      </SimpleGrid>
      <Group gap="xs">
        <Text size="sm" c="dimmed">
          Preview
        </Text>
        <DefectChip defect={{ typeId: d.id, group: d.group, name: d.name || 'Name' }} types={new Map(d.id ? [[d.id, { id: d.id, group: d.group, name: d.name || 'Name', shortName: d.shortName, color: d.color, builtIn: d.builtIn }]] : [])} />
      </Group>
      {save.isError && (
        <Text size="sm" c="red">
          {errorMessage(save.error)}
        </Text>
      )}
      <Group justify="flex-end">
        <Button variant="default" onClick={onClose}>
          Cancel
        </Button>
        <Button onClick={submit} disabled={!valid} loading={save.isPending} data-testid="defect-type-save">
          {d.id ? 'Save' : 'Add type'}
        </Button>
      </Group>
    </Stack>
  );
}

function DefectTypes({ project, canWrite }: { project: string; canWrite: boolean }) {
  const q = useDefectTypes(project);
  const del = useDeleteDefectType(project);
  const [draft, setDraft] = useState<TypeDraft | null>(null);
  const [confirm, setConfirm] = useState<DefectType | null>(null);
  const types = useMemo(() => new Map((q.data ?? []).map((t) => [t.id, t] as const)), [q.data]);
  return (
    <Stack gap="md">
      <Group justify="space-between" wrap="wrap" gap="xs">
        <Text size="sm" c="dimmed" style={{ flex: '1 1 300px' }}>
          Five built-in groups; add your own subtypes under a group to triage more precisely.
        </Text>
        {canWrite && (
          <Button leftSection={<IconPlus size={16} />} onClick={() => setDraft({ builtIn: false, group: 'productBug', name: '', shortName: '', color: '#f06595' })} data-testid="defect-type-add">
            Add type
          </Button>
        )}
      </Group>
      <QueryState query={q}>
        {() => (
          <Stack gap="sm">
            {DEFECT_GROUPS.map((g) => (
              <Card key={g} withBorder radius="md" padding="sm" data-testid={`defect-group-${g}`}>
                <Group gap="xs" mb={6}>
                  <span aria-hidden style={{ width: 10, height: 10, borderRadius: 3, background: GROUP_COLOR[g], display: 'inline-block' }} />
                  <Text fw={700} size="sm">
                    {GROUP_LABEL[g]}
                  </Text>
                </Group>
                <Stack gap={4}>
                  {(q.data ?? [])
                    .filter((t) => t.group === g)
                    .map((t) => (
                      <Group key={t.id} justify="space-between" wrap="nowrap" gap="xs" className="fp-tree-row" data-testid="defect-type-row">
                        <Group gap="xs" wrap="nowrap" style={{ minWidth: 0 }}>
                          <DefectChip defect={{ typeId: t.id, group: t.group, name: t.name }} types={types} />
                          <Text size="xs" c="dimmed" className="fp-mono">
                            {t.shortName}
                          </Text>
                          {t.builtIn && (
                            <Badge size="xs" variant="outline" color="gray" style={{ textTransform: 'none' }}>
                              built-in
                            </Badge>
                          )}
                        </Group>
                        <Group gap={4} wrap="nowrap">
                          <CopyValue value={t.id} label={`Copy the id of ${t.name}`} />
                          {canWrite && (
                          <>
                            <Tooltip label={`Edit ${t.name}`}>
                              <ActionIcon variant="subtle" aria-label={`Edit ${t.name}`} onClick={() => setDraft({ id: t.id, builtIn: t.builtIn, group: t.group, name: t.name, shortName: t.shortName, color: /^#[0-9a-fA-F]{6}$/.test(t.color) ? t.color : GROUP_COLOR[t.group] })}>
                                <IconEdit size={16} />
                              </ActionIcon>
                            </Tooltip>
                            {t.builtIn ? (
                              <span aria-hidden style={{ width: 28, flex: 'none' }} />
                            ) : (
                              <Tooltip label={`Delete ${t.name}`}>
                                <ActionIcon variant="subtle" color="red" aria-label={`Delete ${t.name}`} onClick={() => setConfirm(t)}>
                                  <IconTrash size={16} />
                                </ActionIcon>
                              </Tooltip>
                            )}
                          </>
                          )}
                        </Group>
                      </Group>
                    ))}
                </Stack>
              </Card>
            ))}
          </Stack>
        )}
      </QueryState>
      <Modal opened={!!draft} onClose={() => setDraft(null)} title={draft?.id ? 'Edit defect type' : 'New defect type'}>
        {draft && <DefectTypeEditor project={project} draft={draft} onClose={() => setDraft(null)} />}
      </Modal>
      <Modal opened={!!confirm} onClose={() => setConfirm(null)} title="Delete the defect type?">
        {confirm && (
          <Stack gap="sm">
            <Text size="sm">Results triaged as {confirm.name} fall back to their group. Rules that set it stop setting a type.</Text>
            <Group justify="flex-end">
              <Button variant="default" onClick={() => setConfirm(null)}>
                Cancel
              </Button>
              <Button
                color="red"
                loading={del.isPending}
                onClick={() =>
                  del.mutate(confirm.id, {
                    onSuccess: () => {
                      notifications.show({ color: 'green', message: `${confirm.name} deleted` });
                      setConfirm(null);
                    },
                    onError: (e) => notifications.show({ color: 'red', title: 'Not deleted', message: errorMessage(e) }),
                  })
                }
              >
                Delete
              </Button>
            </Group>
          </Stack>
        )}
      </Modal>
    </Stack>
  );
}

const SAMPLE_KEY: Record<IssueTrackerType, string> = { jira: 'SHOP-123', gitlab: '42', github: '#42' };

const TRACKER_DEFAULT: Record<IssueTrackerType, IssueTracker> = {
  jira: { type: 'jira', urlTemplate: 'https://jira.example.com/browse/{key}', keyRegex: '^[A-Z][A-Z0-9]+-\\d+$' },
  gitlab: { type: 'gitlab', urlTemplate: 'https://gitlab.example.com/group/project/-/issues/{key}', keyRegex: '^#?\\d+$' },
  github: { type: 'github', urlTemplate: 'https://github.com/owner/repo/issues/{key}', keyRegex: '^#?\\d+$' },
};

/** Sends the Jira ids only when set on a Jira tracker. */
function cleanTracker(t: IssueTracker): IssueTracker {
  const { projectId, issueTypeId, ...rest } = t;
  if (t.type !== 'jira') return rest;
  return { ...rest, ...(projectId ? { projectId } : {}), ...(issueTypeId ? { issueTypeId } : {}) };
}

const JIRA_ID = /^\d{1,18}$/;

function trackerErrors(t: IssueTracker): { url: string | null; regex: string | null } {
  let url: string | null = null;
  if (!t.urlTemplate.includes('{key}')) url = 'Must contain {key}';
  else if (!HTTP_URL.test(t.urlTemplate.replace('{key}', 'X'))) url = 'Must be an http(s) URL';
  else if (t.urlTemplate.length > 500) url = 'At most 500 characters';
  let regex: string | null = null;
  if (t.keyRegex.length > 200) regex = 'At most 200 characters';
  else {
    try {
      new RegExp(t.keyRegex);
    } catch {
      regex = 'Not a valid regular expression';
    }
  }
  return { url, regex };
}

const REPO_PATH = /^[A-Za-z0-9._-]+(\/[A-Za-z0-9._-]+)+$/;
const ORIGIN = /^https?:\/\/[^\s/?#@]+\/?$/i;
const PURPOSES: { value: SecretPurpose; label: string }[] = [
  { value: 'vcs', label: 'GitLab or GitHub token' },
  { value: 'fastmock', label: 'FastMock token' },
  { value: 'ci', label: 'CI token for reruns' },
  { value: 'slack', label: 'Slack webhook URL' },
  { value: 'teams', label: 'Microsoft Teams workflow URL' },
  { value: 'webhook', label: 'Webhook signing key or URL' },
  { value: 'telegram', label: 'Telegram bot token' },
];

function secretOptions(names: string[], current: string | null | undefined): string[] {
  return current && !names.includes(current) ? [...names, current] : names;
}

function IntegrationsForm({ project, settings, canWrite }: { project: string; settings: ProjectSettings; canWrite: boolean }) {
  const [trackers, setTrackers] = useState<IssueTracker[]>(settings.issueTrackers);
  const [vcs, setVcs] = useState<VcsSettings | null>(settings.vcs ?? null);
  const [fm, setFm] = useState(settings.fastmock ?? null);
  const [rr, setRr] = useState<RerunSettings | null>(settings.rerun ?? null);
  const secrets = useSecrets(project, canWrite);
  const names = (secrets.data ?? []).map((s) => s.name);
  const { run, pending } = useSave(project);

  const tErrors = trackers.map(trackerErrors);
  const vcsUrlError = vcs && !HTTP_URL.test(vcs.url) ? 'Must be an http(s) URL' : null;
  const vcsTokenError = vcs && !vcs.tokenSecretRef ? 'Choose the secret with the API token' : null;
  const vcsRepoError = vcs && !REPO_PATH.test(vcs.repository) ? (vcs.provider === 'github' ? 'owner/repo' : 'group/project, subgroups allowed') : null;
  const rrUrlError = rr && !HTTP_URL.test(rr.url) ? 'Must be an http(s) URL' : null;
  const rrRepoError = rr && rr.provider !== 'webhook' && !REPO_PATH.test(rr.repository ?? '') ? (rr.provider === 'github' ? 'owner/repo' : 'group/project, subgroups allowed') : null;
  const rrWorkflowError = rr?.provider === 'github' && !rr.workflow ? 'The workflow file, for example tests.yml' : null;
  const rrTokenError = rr && rr.provider !== 'webhook' && !rr.tokenSecretRef ? 'Choose the secret with the token' : null;
  const fmUrlError = fm && !HTTP_URL.test(fm.url) ? 'Must be an http(s) URL' : null;
  const valid = tErrors.every((e) => !e.url && !e.regex) && trackers.every((t) => t.type !== 'jira' || ((!t.projectId || JIRA_ID.test(t.projectId)) && (!t.issueTypeId || JIRA_ID.test(t.issueTypeId)))) && !vcsUrlError && !vcsTokenError && !vcsRepoError && !fmUrlError && !rrUrlError && !rrRepoError && !rrWorkflowError && !rrTokenError && trackers.length <= 10;

  const setTracker = (i: number, t: Partial<IssueTracker>) => setTrackers(trackers.map((x, j) => (j === i ? { ...x, ...t } : x)));

  return (
    <Fieldset variant="unstyled" disabled={!canWrite}>
      <Stack gap="md">
        <Section title="Issue trackers" description="Triage turns an issue key into a link with these templates; {key} is replaced with the key.">
          {trackers.length === 0 && (
            <Text size="sm" c="dimmed">
              No trackers. Triage still accepts full issue URLs.
            </Text>
          )}
          {trackers.map((t, i) => {
            const preview = issueUrlFor(t.urlTemplate, t.keyRegex, SAMPLE_KEY[t.type]);
            return (
              <Card key={i} withBorder radius="sm" padding="sm" data-testid="tracker-row">
                <Stack gap="xs">
                  <SimpleGrid cols={{ base: 1, sm: 3 }} spacing="xs">
                    <Select
                      label="Type"
                      data={[
                        { value: 'jira', label: 'Jira' },
                        { value: 'gitlab', label: 'GitLab' },
                        { value: 'github', label: 'GitHub' },
                      ]}
                      value={t.type}
                      onChange={(v) => v && setTracker(i, { type: v })}
                      allowDeselect={false}
                      comboboxProps={{ withinPortal: true }}
                    />
                    <TextInput label="URL template" value={t.urlTemplate} onChange={(e) => setTracker(i, { urlTemplate: e.currentTarget.value })} error={tErrors[i]?.url} classNames={{ input: 'fp-mono' }} />
                    <TextInput label="Key pattern" value={t.keyRegex} onChange={(e) => setTracker(i, { keyRegex: e.currentTarget.value })} error={tErrors[i]?.regex} classNames={{ input: 'fp-mono' }} />
                  </SimpleGrid>
                  {t.type === 'jira' && (
                    <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="xs">
                      <TextInput
                        label="Jira project id (optional)"
                        description="Numeric id; with the issue type id the quick issue link opens a prefilled create screen"
                        value={t.projectId ?? ''}
                        onChange={(e) => setTracker(i, { projectId: e.currentTarget.value.trim() || null })}
                        error={t.projectId && !JIRA_ID.test(t.projectId) ? 'Digits only' : null}
                        classNames={{ input: 'fp-mono' }}
                        data-testid="tracker-jira-project"
                      />
                      <TextInput
                        label="Jira issue type id (optional)"
                        description="Numeric id, for example the id of Bug"
                        value={t.issueTypeId ?? ''}
                        onChange={(e) => setTracker(i, { issueTypeId: e.currentTarget.value.trim() || null })}
                        error={t.issueTypeId && !JIRA_ID.test(t.issueTypeId) ? 'Digits only' : null}
                        classNames={{ input: 'fp-mono' }}
                        data-testid="tracker-jira-issuetype"
                      />
                    </SimpleGrid>
                  )}
                  <Group justify="space-between" wrap="nowrap" gap="xs">
                    <Text size="xs" c="dimmed" className="fh-truncate" style={{ minWidth: 0 }} data-testid="tracker-preview">
                      <span className="fp-mono">{SAMPLE_KEY[t.type]}</span> → {preview ?? 'the sample key does not match the pattern'}
                    </Text>
                    {canWrite && (
                      <ActionIcon variant="subtle" color="red" aria-label="Remove the tracker" onClick={() => setTrackers(trackers.filter((_, j) => j !== i))}>
                        <IconTrash size={16} />
                      </ActionIcon>
                    )}
                  </Group>
                </Stack>
              </Card>
            );
          })}
          {canWrite && trackers.length < 10 && (
            <Group gap="xs">
              {(['jira', 'gitlab', 'github'] as const).map((k) => (
                <Button key={k} size="xs" variant="light" leftSection={<IconPlus size={14} />} onClick={() => setTrackers([...trackers, { ...TRACKER_DEFAULT[k] }])}>
                  {k === 'jira' ? 'Jira' : k === 'gitlab' ? 'GitLab' : 'GitHub'}
                </Button>
              ))}
            </Group>
          )}
        </Section>
        <Section title="Merge request comments" description="When a launch of the repository below finishes, FastProof posts or updates one comment and a commit status on its merge request. The token secret needs purpose vcs and the origin of the API URL.">
          <Switch
            label="Enabled"
            checked={!!vcs}
            onChange={(e) => setVcs(e.currentTarget.checked ? { provider: 'gitlab', url: 'https://gitlab.example.com', repository: '', tokenSecretRef: '', comment: true, status: true } : null)}
            data-testid="vcs-enabled"
          />
          {vcs && (
            <>
              <SimpleGrid cols={{ base: 1, sm: 3 }} spacing="xs">
                <Select
                  label="Provider"
                  data={[
                    { value: 'gitlab', label: 'GitLab' },
                    { value: 'github', label: 'GitHub' },
                  ]}
                  value={vcs.provider}
                  onChange={(v) => v && setVcs({ ...vcs, provider: v })}
                  allowDeselect={false}
                  comboboxProps={{ withinPortal: true }}
                />
                <TextInput label="API base URL" value={vcs.url} onChange={(e) => setVcs({ ...vcs, url: e.currentTarget.value })} error={vcsUrlError} />
                <Select label="Token secret" placeholder="Choose a secret" data={secretOptions(names, vcs.tokenSecretRef)} value={vcs.tokenSecretRef || null} onChange={(v) => setVcs({ ...vcs, tokenSecretRef: v ?? '' })} error={vcsTokenError} comboboxProps={{ withinPortal: true }} data-testid="vcs-token" />
              </SimpleGrid>
              <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="xs">
                <TextInput label="Repository" description="The only repository FastProof posts to" placeholder={vcs.provider === 'github' ? 'owner/repo' : 'group/app'} value={vcs.repository} onChange={(e) => setVcs({ ...vcs, repository: e.currentTarget.value.trim() })} error={vcsRepoError} data-testid="vcs-repository" />
                <TagsInput label="Only source branches" description="Empty: every branch" placeholder="feature/*" value={vcs.sourceBranches ?? []} onChange={(v) => setVcs({ ...vcs, sourceBranches: v.length ? v : null })} maxTags={50} />
              </SimpleGrid>
              <Group gap="lg">
                <Switch label="Comment with the summary" checked={vcs.comment} onChange={(e) => setVcs({ ...vcs, comment: e.currentTarget.checked })} />
                <Switch label="Commit status" checked={vcs.status} onChange={(e) => setVcs({ ...vcs, status: e.currentTarget.checked })} />
              </Group>
            </>
          )}
        </Section>
        <Section title="FastMock" description="Shows the mock calls a test made (by trace id or time window) on the result page. A token secret needs purpose fastmock and the origin of the FastMock URL.">
          <Switch label="Enabled" checked={!!fm} onChange={(e) => setFm(e.currentTarget.checked ? { url: 'https://fastmock.example.com', tokenSecretRef: null } : null)} />
          {fm && (
            <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="xs">
              <TextInput label="FastMock URL" value={fm.url} onChange={(e) => setFm({ ...fm, url: e.currentTarget.value })} error={fmUrlError} />
              <Select label="Token secret" placeholder="Choose a secret" data={secretOptions(names, fm.tokenSecretRef)} value={fm.tokenSecretRef ?? null} onChange={(v) => setFm({ ...fm, tokenSecretRef: v })} clearable comboboxProps={{ withinPortal: true }} />
            </SimpleGrid>
          )}
        </Section>
        <Section title="Rerun in CI" description="Adds Rerun failed and Rerun selected to a launch: FastProof makes a testplan.json and triggers the CI once. The token secret needs purpose ci, the origin of the URL and a scope naming the provider, repository and workflow: changing any of them means writing the secret again. The signing secret (webhook) signs the body with HMAC-SHA256.">
          <Switch
            label="Enabled"
            checked={!!rr}
            onChange={(e) => setRr(e.currentTarget.checked ? { provider: 'github', url: 'https://api.github.com', repository: '', workflow: 'tests.yml', ref: 'main', tokenSecretRef: '' } : null)}
            data-testid="rerun-enabled"
          />
          {rr && (
            <>
              <SimpleGrid cols={{ base: 1, sm: 3 }} spacing="xs">
                <Select
                  label="CI"
                  data={[
                    { value: 'github', label: 'GitHub Actions (workflow_dispatch)' },
                    { value: 'gitlab', label: 'GitLab pipeline trigger' },
                    { value: 'webhook', label: 'Webhook' },
                  ]}
                  value={rr.provider}
                  onChange={(v) => {
                    const provider: RerunProvider | null = v === 'github' || v === 'gitlab' || v === 'webhook' ? v : null;
                    if (!provider) return;
                    setRr(provider === 'webhook' ? { provider, url: '', tokenSecretRef: rr.tokenSecretRef } : { provider, url: provider === 'github' ? 'https://api.github.com' : 'https://gitlab.example.com', repository: rr.repository ?? '', workflow: provider === 'github' ? 'tests.yml' : undefined, ref: rr.ref ?? 'main', tokenSecretRef: rr.tokenSecretRef });
                  }}
                  allowDeselect={false}
                  comboboxProps={{ withinPortal: true }}
                  data-testid="rerun-provider"
                />
                <TextInput label={rr.provider === 'webhook' ? 'Webhook URL' : 'API base URL'} value={rr.url} onChange={(e) => setRr({ ...rr, url: e.currentTarget.value })} error={rrUrlError} data-testid="rerun-url" />
                <Select label="Token secret" placeholder="Choose a secret" data={secretOptions(names, rr.tokenSecretRef)} value={rr.tokenSecretRef || null} onChange={(v) => setRr({ ...rr, tokenSecretRef: v ?? '' })} clearable={rr.provider === 'webhook'} error={rrTokenError} comboboxProps={{ withinPortal: true }} data-testid="rerun-token" />
              </SimpleGrid>
              {rr.provider === 'webhook' && (
                <Select label="Signing secret" description="Signs every call with HMAC-SHA256 (X-FastProof-Signature); the receiver should verify it" placeholder="Choose a secret" data={secretOptions(names, rr.signingSecretRef)} value={rr.signingSecretRef || null} onChange={(v) => setRr({ ...rr, signingSecretRef: v })} clearable comboboxProps={{ withinPortal: true }} data-testid="rerun-signing" />
              )}
              {rr.provider !== 'webhook' && (
                <SimpleGrid cols={{ base: 1, sm: rr.provider === 'github' ? 3 : 2 }} spacing="xs">
                  <TextInput label="Repository" placeholder={rr.provider === 'github' ? 'owner/repo' : 'group/app'} value={rr.repository ?? ''} onChange={(e) => setRr({ ...rr, repository: e.currentTarget.value.trim() })} error={rrRepoError} data-testid="rerun-repository" />
                  {rr.provider === 'github' && <TextInput label="Workflow file" placeholder="tests.yml" value={rr.workflow ?? ''} onChange={(e) => setRr({ ...rr, workflow: e.currentTarget.value.trim() })} error={rrWorkflowError} data-testid="rerun-workflow" />}
                  <TextInput label="Branch or tag" description="Empty: main" placeholder="main" value={rr.ref ?? ''} onChange={(e) => setRr({ ...rr, ref: e.currentTarget.value.trim() || undefined })} />
                </SimpleGrid>
              )}
            </>
          )}
        </Section>
        {canWrite && (
          <Group>
            <Button onClick={() => run({ ...settings, issueTrackers: trackers.map(cleanTracker), vcs, fastmock: fm, rerun: rr && { ...rr, tokenSecretRef: rr.tokenSecretRef || null, signingSecretRef: rr.signingSecretRef || null } }, 'Integrations')} loading={pending} disabled={!valid} data-testid="integrations-save">
              Save
            </Button>
          </Group>
        )}
      </Stack>
    </Fieldset>
  );
}

function Secrets({ project, canWrite }: { project: string; canWrite: boolean }) {
  const q = useSecrets(project, canWrite);
  const write = useWriteSecret(project);
  const [name, setName] = useState('');
  const [value, setValue] = useState('');
  const [purpose, setPurpose] = useState<SecretPurpose | null>(null);
  const [origin, setOrigin] = useState('');
  const [scope, setScope] = useState('');
  if (!canWrite) {
    return <EmptyState title="Secrets are managed by project admins" description="Writing and listing secrets needs the settings:write permission." />;
  }
  const nameError = name && !SECRET_NAME.test(name) ? 'Lowercase letters, digits, . _ -; up to 63 characters' : null;
  const valueError = value.length > 8192 ? 'At most 8192 characters' : purpose === 'teams' && value && !/^https:\/\/\S+$/i.test(value.trim()) ? 'The workflow URL: https://… (its origin is bound automatically)' : null;
  const needsOrigin = purpose === 'vcs' || purpose === 'fastmock' || purpose === 'ci';
  const originError = origin && !ORIGIN.test(origin) ? 'An origin such as https://gitlab.example.com' : needsOrigin && !origin ? 'Required for this purpose' : null;
  const scopeError = purpose === 'ci' && !/^(github:[^:\s]+:[^:\s]+|gitlab:[^:\s]+|webhook:https?:\/\/\S+)$/.test(scope) ? 'github:owner/repo:workflow.yml, gitlab:group/project or webhook:<URL>' : null;
  const valid = SECRET_NAME.test(name) && value.length > 0 && !valueError && !!purpose && !originError && !scopeError;
  const exists = (q.data ?? []).some((s) => s.name === name);
  const submit = () => {
    if (!valid) return;
    write.mutate(
      { name, value, purpose, origin: origin.trim() || null, scope: purpose === 'ci' ? scope : null },
      {
        onSuccess: () => {
          notifications.show({ color: 'green', message: `Secret ${name} saved` });
          setValue('');
          setName('');
          setOrigin('');
          setScope('');
        },
        onError: (e) => {
          setValue('');
          notifications.show({ color: 'red', title: 'Secret not saved', message: errorMessage(e) });
        },
      },
    );
  };
  return (
    <Stack gap="md">
      <Alert color="gray" variant="light" icon={<IconLock size={18} />} title="Write-only">
        Secret values are encrypted on the server and never shown again, not even to admins. Settings refer to them by name (tokens for merge request comments, FastMock, notification signing keys and URLs). Each secret is used only for its purpose and sent only to its origin: to change a value or point it at another URL, write it again under the same name.
      </Alert>
      <Card withBorder radius="md" padding="md">
        <Stack gap="sm">
          <Title order={5}>Write a secret</Title>
          <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="sm">
            <TextInput label="Name" placeholder="gitlab-token" value={name} onChange={(e) => setName(e.currentTarget.value.trim())} error={nameError} maxLength={63} data-testid="secret-name" />
            <PasswordInput label="Value" value={value} onChange={(e) => setValue(e.currentTarget.value)} error={valueError} autoComplete="new-password" data-testid="secret-value" />
            <Select label="Purpose" placeholder="Choose" data={PURPOSES} value={purpose} onChange={(v) => setPurpose(PURPOSES.find((p) => p.value === v)?.value ?? null)} allowDeselect={false} comboboxProps={{ withinPortal: true }} data-testid="secret-purpose" />
            <TextInput label="Origin" description={needsOrigin ? 'Where the secret is sent, e.g. https://gitlab.example.com' : 'Optional: the only origin it may be sent to'} placeholder="https://gitlab.example.com" value={origin} onChange={(e) => setOrigin(e.currentTarget.value.trim())} error={originError} data-testid="secret-origin" />
            {purpose === 'ci' && <TextInput label="Scope" description="What the token may be used for. The rerun settings must name exactly this target" placeholder="github:owner/repo:tests.yml" value={scope} onChange={(e) => setScope(e.currentTarget.value.trim())} error={scopeError} data-testid="secret-scope" />}
          </SimpleGrid>
          {exists && (
            <Text size="xs" c="var(--fh-semantic-warning-fg)">
              {name} exists; saving replaces its value.
            </Text>
          )}
          <Group>
            <Button onClick={submit} disabled={!valid} loading={write.isPending} data-testid="secret-save">
              {exists ? 'Replace' : 'Save secret'}
            </Button>
          </Group>
        </Stack>
      </Card>
      <QueryState query={q}>
        {() =>
          q.data?.length ? (
            <Card withBorder radius="md" padding={0}>
              <div className="fp-table-wrap">
                <Table verticalSpacing={6} horizontalSpacing="md" fz="sm" data-testid="secrets-table">
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th>Name</Table.Th>
                      <Table.Th>Purpose</Table.Th>
                      <Table.Th>Origin</Table.Th>
                      <Table.Th>Value</Table.Th>
                      <Table.Th>Updated</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {q.data.map((s) => (
                      <Table.Tr key={s.name} data-testid="secret-row">
                        <Table.Td className="fp-mono">{s.name}</Table.Td>
                        <Table.Td>{s.purpose ?? <Text span size="sm" c="var(--fh-semantic-warning-fg)">none, write again</Text>}</Table.Td>
                        <Table.Td className="fp-mono" c="dimmed">{s.origin ?? 'any'}</Table.Td>
                        <Table.Td c="dimmed">••••••••</Table.Td>
                        <Table.Td c="dimmed" title={`${formatDateTime(s.updatedAt)}${s.updatedBy ? ` by ${s.updatedBy}` : ''}`}>
                          {formatRelative(s.updatedAt)}
                        </Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </div>
            </Card>
          ) : (
            <EmptyState title="No secrets yet" />
          )
        }
      </QueryState>
    </Stack>
  );
}

export default function SettingsPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const canWrite = useCan('settings:write', key);
  const canReadCases = useCan('cases:read', key);
  const settings = useSettings(key);
  const [sp, setSp] = useSearchParams();
  const tab = sp.get('tab') ?? 'general';
  const setTab = (t: string | null) =>
    setSp(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (!t || t === 'general') next.delete('tab');
        else next.set('tab', t);
        return next;
      },
      { replace: true },
    );
  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Settings' }]}
        title="Project settings"
        badges={
          !canWrite ? (
            <Badge variant="outline" color="gray" size="sm" style={{ textTransform: 'none' }} data-testid="read-only">
              read-only
            </Badge>
          ) : undefined
        }
        subtitle="Flaky detection, retention, defect types, integrations and secrets of this project."
        actions={
          canReadCases ? (
            <Button size="xs" variant="default" component={Link} to={`/p/${key}/settings/case-fields`} leftSection={<IconForms size={14} />} data-testid="settings-case-fields">
              Case fields
            </Button>
          ) : undefined
        }
      />
      <Tabs value={tab} onChange={setTab} keepMounted={false}>
        <Tabs.List>
          <Tabs.Tab value="general">General</Tabs.Tab>
          <Tabs.Tab value="defects">Defect types</Tabs.Tab>
          <Tabs.Tab value="integrations">Integrations</Tabs.Tab>
          <Tabs.Tab value="secrets">Secrets</Tabs.Tab>
        </Tabs.List>
        <Tabs.Panel value="general" pt="md">
          <QueryState query={settings}>{() => settings.data && <GeneralForm key={JSON.stringify(settings.data)} project={key} settings={settings.data} canWrite={canWrite} />}</QueryState>
        </Tabs.Panel>
        <Tabs.Panel value="defects" pt="md">
          <DefectTypes project={key} canWrite={canWrite} />
        </Tabs.Panel>
        <Tabs.Panel value="integrations" pt="md">
          <QueryState query={settings}>{() => settings.data && <IntegrationsForm key={JSON.stringify(settings.data)} project={key} settings={settings.data} canWrite={canWrite} />}</QueryState>
        </Tabs.Panel>
        <Tabs.Panel value="secrets" pt="md">
          <Secrets project={key} canWrite={canWrite} />
        </Tabs.Panel>
      </Tabs>
    </>
  );
}
