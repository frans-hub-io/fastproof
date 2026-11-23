import { Button, Code, Group, Select, SimpleGrid, Stack, Switch, TagsInput, Text, Textarea, TextInput } from '@mantine/core';
import { IconPlugConnected } from '@tabler/icons-react';
import { errorMessage } from '../api/client';
import type { TrackerApi } from '../api/trackers';
import { useTestTracker } from '../api/trackers';
import type { IssueTracker } from '../api/types-next';
import { trackerApiErrors } from '../lib/trackers';

const PLACEHOLDERS =
  'name, fullName, status, messageLine, launch, launchUrl, resultUrl, branch, environment, fingerprint, count, defect, project, user; blocks on their own line: message, trace, tests';

const API_DEFAULT: Record<IssueTracker['type'], TrackerApi> = {
  jira: {
    url: 'https://example.atlassian.net',
    tokenSecretRef: '',
    project: 'SHOP',
  },
  gitlab: {
    url: 'https://gitlab.example.com',
    tokenSecretRef: '',
    project: 'group/app',
  },
  github: {
    url: 'https://api.github.com',
    tokenSecretRef: '',
    project: 'owner/repo',
  },
  azure: {
    url: 'https://dev.azure.com/example',
    tokenSecretRef: '',
    project: 'Shop',
  },
};

export function TrackerApiFields({
  project,
  tracker,
  secrets,
  saved,
  onChange,
}: {
  project: string;
  tracker: IssueTracker;
  secrets: string[];
  saved: boolean;
  onChange: (t: Partial<IssueTracker>) => void;
}) {
  const test = useTestTracker(project);
  const a = tracker.api;
  const errors = trackerApiErrors(tracker);
  const set = (patch: Partial<TrackerApi>) => a && onChange({ api: { ...a, ...patch } });
  const options = (cur?: string | null) => (cur && !secrets.includes(cur) ? [...secrets, cur] : secrets);
  const hook = tracker.id ? `${window.location.origin}/api/v1/hooks/trackers/${project}/${tracker.id}` : '';
  return (
    <Stack gap="xs" data-testid="tracker-api">
      <Switch
        label="Create issues from FastProof and follow their state (API connection)"
        checked={!!a}
        onChange={(e) =>
          onChange(
            e.currentTarget.checked
              ? {
                  api: { ...API_DEFAULT[tracker.type] },
                  id: tracker.id ?? tracker.type,
                }
              : { api: null },
          )
        }
        data-testid="tracker-api-enabled"
      />
      {a && (
        <>
          <SimpleGrid cols={{ base: 1, sm: 3 }} spacing="xs">
            <TextInput
              label="Id"
              description="Issues and the webhook URL refer to it"
              value={tracker.id ?? ''}
              onChange={(e) => onChange({ id: e.currentTarget.value.trim() })}
              error={errors.id}
              classNames={{ input: 'fp-mono' }}
              data-testid="tracker-id"
            />
            <TextInput
              label="API URL"
              description={
                tracker.type === 'jira'
                  ? 'The site or server base URL'
                  : tracker.type === 'github'
                    ? 'https://api.github.com or …/api/v3'
                    : tracker.type === 'azure'
                      ? 'https://dev.azure.com/organization'
                      : 'The GitLab base URL'
              }
              value={a.url}
              onChange={(e) => set({ url: e.currentTarget.value })}
              error={errors.url}
              data-testid="tracker-api-url"
            />
            <Select
              label="Token secret"
              description="Purpose tracker, bound to the API origin"
              placeholder="Choose a secret"
              data={options(a.tokenSecretRef)}
              value={a.tokenSecretRef || null}
              onChange={(v) => set({ tokenSecretRef: v ?? '' })}
              error={errors.token}
              comboboxProps={{ withinPortal: true }}
              data-testid="tracker-api-token"
            />
          </SimpleGrid>
          <SimpleGrid cols={{ base: 1, sm: 3 }} spacing="xs">
            <TextInput
              label={tracker.type === 'jira' ? 'Project key' : tracker.type === 'azure' ? 'Project' : 'Repository'}
              value={a.project}
              onChange={(e) => set({ project: e.currentTarget.value })}
              error={errors.project}
              data-testid="tracker-api-project"
            />
            {tracker.type === 'jira' && (
              <Select
                label="Jira"
                data={[
                  { value: 'cloud', label: 'Cloud (email:API token)' },
                  { value: 'server', label: 'Server or Data Center (PAT)' },
                ]}
                value={a.deployment ?? 'cloud'}
                onChange={(v) => set({ deployment: v === 'server' ? 'server' : null })}
                allowDeselect={false}
                comboboxProps={{ withinPortal: true }}
                data-testid="tracker-api-deployment"
              />
            )}
            {(tracker.type === 'jira' || tracker.type === 'azure') && (
              <TextInput label={tracker.type === 'jira' ? 'Issue type' : 'Work item type'} placeholder="Bug" value={a.issueType ?? ''} onChange={(e) => set({ issueType: e.currentTarget.value })} />
            )}
            <TagsInput label={tracker.type === 'azure' ? 'Tags' : 'Labels'} placeholder="fastproof" value={a.labels ?? []} onChange={(v) => set({ labels: v })} maxTags={10} />
          </SimpleGrid>
          <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="xs">
            <Select
              label="State sync"
              data={[
                { value: 'poll', label: 'Read open issues regularly' },
                {
                  value: 'webhook',
                  label: 'On the tracker webhook (and daily)',
                },
                { value: 'off', label: 'Off' },
              ]}
              value={a.sync ?? 'poll'}
              onChange={(v) => set({ sync: v ?? 'poll' })}
              allowDeselect={false}
              comboboxProps={{ withinPortal: true }}
              data-testid="tracker-api-sync"
            />
            {a.sync === 'webhook' && (
              <Select
                label="Webhook secret"
                description="Purpose webhook"
                placeholder="Choose a secret"
                data={options(a.webhookSecretRef)}
                value={a.webhookSecretRef ?? null}
                onChange={(v) => set({ webhookSecretRef: v })}
                error={errors.webhook}
                comboboxProps={{ withinPortal: true }}
              />
            )}
          </SimpleGrid>
          {a.sync === 'webhook' && hook && (
            <Text size="xs" c="dimmed" style={{ wordBreak: 'break-all' }}>
              Webhook URL for the tracker: <Code>{hook}</Code>
            </Text>
          )}
          {tracker.type !== 'github' && (
            <Switch label="Attach screenshots of the result by default" checked={!!a.attachScreenshots} onChange={(e) => set({ attachScreenshots: e.currentTarget.checked })} />
          )}
          <TextInput
            label="Title template"
            placeholder="{{name}}: {{messageLine}}"
            value={a.titleTemplate ?? ''}
            onChange={(e) => set({ titleTemplate: e.currentTarget.value })}
            maxLength={255}
            classNames={{ input: 'fp-mono' }}
          />
          <Textarea
            label="Description template"
            description={`Empty: the default. Placeholders: ${PLACEHOLDERS}`}
            autosize
            minRows={2}
            maxRows={10}
            maxLength={4096}
            value={a.bodyTemplate ?? ''}
            onChange={(e) => set({ bodyTemplate: e.currentTarget.value })}
            classNames={{ input: 'fp-mono' }}
          />
          {tracker.type === 'azure' && <TagsInput label="Closed states" placeholder="Closed, Done, Removed" value={a.closedStates ?? []} onChange={(v) => set({ closedStates: v })} maxTags={10} />}
          <Group gap="xs">
            <Button
              size="xs"
              variant="light"
              leftSection={<IconPlugConnected size={14} />}
              disabled={!saved || !tracker.id}
              loading={test.isPending}
              onClick={() => tracker.id && test.mutate(tracker.id)}
              data-testid="tracker-test"
            >
              Test the connection
            </Button>
            {!saved && (
              <Text size="xs" c="dimmed">
                Save first; the test uses the saved settings.
              </Text>
            )}
            {test.data && (
              <Text size="xs" c={test.data.ok ? 'green' : 'red'} data-testid="tracker-test-result">
                {test.data.ok ? `Connected: ${test.data.project ?? ''}` : test.data.error}
              </Text>
            )}
            {test.error && (
              <Text size="xs" c="red">
                {errorMessage(test.error)}
              </Text>
            )}
          </Group>
        </>
      )}
    </Stack>
  );
}
