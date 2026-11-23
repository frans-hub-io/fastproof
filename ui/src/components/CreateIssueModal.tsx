import { Alert, Anchor, Button, Code, Group, Modal, ScrollArea, SegmentedControl, Select, Stack, Switch, Text, Textarea, TextInput } from '@mantine/core';
import { useMediaQuery } from '@mantine/hooks';
import { IconBug, IconExternalLink, IconLink } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { errorMessage } from '../api/client';
import { useSettings } from '../api/stageC';
import type { CreateTrackerIssue, TrackerIssueCreated } from '../api/trackers';
import { TRACKER_LABEL, useCreateTrackerIssue, useIssuePreview, useLinkTrackerIssue } from '../api/trackers';
import type { IssueTracker } from '../api/types-next';
import { connectedTrackers, newRequestId } from '../lib/trackers';
import type { TriageTarget } from '../lib/triage';
import { IssueStateBadge } from './IssueState';

function trackerLabel(t: IssueTracker & { id: string }): string {
  const project = t.api?.project ? ` ${t.api.project}` : '';
  return `${TRACKER_LABEL[t.type] ?? t.type}${project} (${t.id})`;
}

function Done({ r, onClose }: { r: TrackerIssueCreated; onClose: () => void }) {
  const i = r.issue;
  return (
    <Stack gap="sm" data-testid="issue-created">
      <Alert color="green" variant="light" title={r.repeated ? 'Already linked' : 'Issue ready'}>
        <Stack gap={6}>
          {i?.url ? (
            <Anchor href={i.url} target="_blank" rel="noopener noreferrer nofollow" fw={600} data-testid="issue-created-link">
              {i.key ?? i.url} {i.title} <IconExternalLink size={12} />
            </Anchor>
          ) : null}
          {i && (
            <Group gap="xs">
              <IssueStateBadge issue={i} />
              <Text size="xs" c="dimmed">
                Linked on {String(r.updatedResults)} result
                {r.updatedResults === 1 ? '' : 's'}
                {r.attached ? `, ${String(r.attached)} screenshot${r.attached === 1 ? '' : 's'} attached` : ''}
              </Text>
            </Group>
          )}
          {r.notes.map((n) => (
            <Text key={n} size="xs" c="dimmed">
              {n}
            </Text>
          ))}
        </Stack>
      </Alert>
      <Group justify="flex-end">
        <Button onClick={onClose} data-testid="issue-done">
          Done
        </Button>
      </Group>
    </Stack>
  );
}

function IssueForm({ project, target, onClose, onCreated }: { project: string; target: TriageTarget; onClose: () => void; onCreated?: () => void }) {
  const settings = useSettings(project);
  const trackers = useMemo(() => connectedTrackers(settings.data?.issueTrackers), [settings.data]);
  const [trackerId, setTrackerId] = useState<string | null>(null);
  const tracker = trackers.find((t) => t.id === trackerId) ?? trackers[0];
  const [mode, setMode] = useState<'create' | 'link'>('create');
  const [title, setTitle] = useState('');
  const [note, setNote] = useState('');
  const [shots, setShots] = useState<boolean | null>(null);
  const [key, setKey] = useState('');
  const [requestId] = useState(newRequestId);
  const [showPreview, setShowPreview] = useState(false);
  const create = useCreateTrackerIssue(project);
  const link = useLinkTrackerIssue(project);
  const scope = target.resultIds?.length ? { resultIds: target.resultIds.slice(0, 100) } : { launchId: target.launchId, fingerprint: target.fingerprint };
  const attach = shots ?? tracker?.api?.attachScreenshots ?? false;
  const body: CreateTrackerIssue | null = tracker
    ? {
        tracker: tracker.id,
        requestId,
        ...scope,
        ...(title.trim() ? { title: title.trim() } : {}),
        ...(note.trim() ? { note } : {}),
        attachScreenshots: attach,
      }
    : null;
  const preview = useIssuePreview(project, showPreview ? body : null);
  const done = create.data ?? link.data;

  if (done)
    return (
      <Done
        r={done}
        onClose={() => {
          onCreated?.();
          onClose();
        }}
      />
    );
  if (settings.isLoading) return <Text size="sm">Loading…</Text>;
  if (!tracker) {
    return (
      <Alert color="gray" variant="light" title="No tracker connection" data-testid="issue-no-tracker">
        Connect a tracker in Project settings, Integrations: give an issue tracker an id and its API settings with a token secret of purpose tracker.
      </Alert>
    );
  }
  const keyOk = tracker.type === 'jira' ? /^[A-Z][A-Z0-9_]*-\d+$/.test(key.trim()) : /^#?[1-9]\d{0,11}$/.test(key.trim());
  const error = create.error ?? link.error;
  return (
    <Stack gap="md" data-testid="issue-form">
      <Text size="sm" c="dimmed">
        {target.label}
      </Text>
      <Group gap="xs" align="flex-end" wrap="wrap">
        {trackers.length > 1 && (
          <Select
            label="Tracker"
            data={trackers.map((t) => ({
              value: t.id,
              label: trackerLabel(t),
            }))}
            value={tracker.id}
            onChange={setTrackerId}
            allowDeselect={false}
            comboboxProps={{ withinPortal: true }}
            style={{ flex: 1, minWidth: 200 }}
            data-testid="issue-tracker"
          />
        )}
        <SegmentedControl
          value={mode}
          onChange={(v) => setMode(v)}
          data={[
            { value: 'create', label: 'New issue' },
            { value: 'link', label: 'Link existing' },
          ]}
          data-testid="issue-mode"
        />
      </Group>
      {trackers.length === 1 && (
        <Text size="xs" c="dimmed">
          {trackerLabel(tracker)}
        </Text>
      )}
      {mode === 'create' ? (
        <>
          <TextInput
            label="Title"
            placeholder="From the template: test name and the first line of the error"
            value={title}
            onChange={(e) => setTitle(e.currentTarget.value)}
            maxLength={255}
            data-testid="issue-title"
          />
          <Textarea
            label="Note"
            description="Put above the generated description, as plain text"
            autosize
            minRows={2}
            maxRows={6}
            maxLength={4000}
            value={note}
            onChange={(e) => setNote(e.currentTarget.value)}
            data-testid="issue-note"
          />
          {tracker.type === 'github' ? (
            <Text size="xs" c="dimmed">
              GitHub has no API for issue attachments; the issue links back to the result and its screenshots.
            </Text>
          ) : (
            <Switch label="Attach screenshots of the last attempt (up to 3)" checked={attach} onChange={(e) => setShots(e.currentTarget.checked)} data-testid="issue-shots" />
          )}
          <Stack gap={6}>
            <Anchor component="button" type="button" size="sm" onClick={() => setShowPreview((v) => !v)} data-testid="issue-preview-toggle">
              {showPreview ? 'Hide the preview' : 'Preview the description'}
            </Anchor>
            {showPreview && (
              <ScrollArea.Autosize mah={260} type="auto" data-testid="issue-preview">
                {preview.isLoading ? (
                  <Text size="sm">Rendering…</Text>
                ) : preview.isError ? (
                  <Text size="sm" c="red">
                    {errorMessage(preview.error)}
                  </Text>
                ) : (
                  <Stack gap={4}>
                    <Text size="sm" fw={600}>
                      {preview.data?.preview.title}
                    </Text>
                    <Code
                      block
                      style={{
                        whiteSpace: 'pre-wrap',
                        wordBreak: 'break-word',
                      }}
                    >
                      {preview.data?.preview.text}
                    </Code>
                    {preview.data?.preview.screenshots.length ? (
                      <Text size="xs" c="dimmed">
                        Screenshots: {preview.data.preview.screenshots.join(', ')}
                      </Text>
                    ) : null}
                  </Stack>
                )}
              </ScrollArea.Autosize>
            )}
          </Stack>
        </>
      ) : (
        <TextInput
          label="Issue"
          placeholder={tracker.type === 'jira' ? 'SHOP-123' : '#123'}
          description="FastProof reads it from the tracker, links it on the results and follows its state"
          value={key}
          onChange={(e) => setKey(e.currentTarget.value)}
          error={key.trim() && !keyOk ? (tracker.type === 'jira' ? 'A Jira key such as SHOP-123' : 'An issue number such as 123') : null}
          maxLength={40}
          data-testid="issue-key"
        />
      )}
      {error && (
        <Alert color="red" variant="light" title={mode === 'create' ? 'Not created' : 'Not linked'} data-testid="issue-error">
          {errorMessage(error)}
        </Alert>
      )}
      <Group justify="flex-end" gap="xs">
        <Button variant="default" onClick={onClose}>
          Cancel
        </Button>
        {mode === 'create' ? (
          <Button leftSection={<IconBug size={16} />} loading={create.isPending} onClick={() => body && create.mutate(body)} data-testid="issue-create">
            Create in {TRACKER_LABEL[tracker.type] ?? tracker.type}
          </Button>
        ) : (
          <Button
            leftSection={<IconLink size={16} />}
            loading={link.isPending}
            disabled={!keyOk}
            onClick={() =>
              link.mutate({
                tracker: tracker.id,
                issueKey: key.trim(),
                ...scope,
              })
            }
            data-testid="issue-link"
          >
            Link
          </Button>
        )}
      </Group>
    </Stack>
  );
}

/** Creates an issue in a connected tracker for results or a failure cluster, or links one. */
export function CreateIssueModal({ project, target, onClose, onCreated }: { project: string; target: TriageTarget | null; onClose: () => void; onCreated?: () => void }) {
  const phone = useMediaQuery('(max-width: 48em)');
  return (
    <Modal opened={!!target} onClose={onClose} title="Issue in tracker" size="lg" fullScreen={phone} styles={{ title: { fontWeight: 700 } }}>
      {target && <IssueForm key={`${target.fingerprint ?? ''}:${(target.resultIds ?? []).join(',')}`} project={project} target={target} onClose={onClose} onCreated={onCreated} />}
    </Modal>
  );
}
