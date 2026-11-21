import { Alert, Anchor, Button, Drawer, Group, SegmentedControl, Select, Stack, Switch, Text, Textarea, TextInput } from '@mantine/core';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconBellOff, IconExternalLink } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { errorMessage } from '../api/client';
import { useDefectTypes, useSettings, useTriage } from '../api/stageC';
import type { TriageRequest } from '../api/types-next';
import { DEFECT_GROUPS } from '../api/types-next';
import { GROUP_LABEL, issueUrlFor } from '../lib/defects';
import { formatDateTime } from '../lib/format';
import { DefectChip } from './Defect';
import type { TriageTarget } from '../lib/triage';

const MUTE_FOR: { value: string; label: string; days: number | null }[] = [
  { value: '1', label: '1 day', days: 1 },
  { value: '7', label: '7 days', days: 7 },
  { value: '30', label: '30 days', days: 30 },
  { value: 'forever', label: 'Until unmuted', days: null },
];

const MAX_COMMENT = 4000;

function TriageForm({ project, target, onDone }: { project: string; target: TriageTarget; onDone: () => void }) {
  const types = useDefectTypes(project);
  const settings = useSettings(project);
  const triage = useTriage(project);
  const trackers = useMemo(() => settings.data?.issueTrackers ?? [], [settings.data]);
  const [typeId, setTypeId] = useState<string | null>(target.defect?.typeId ?? null);
  const [comment, setComment] = useState(target.defect?.comment ?? '');
  const [issue, setIssue] = useState(target.defect?.issueKey ?? target.defect?.issueUrl ?? '');
  const [tracker, setTracker] = useState('0');
  const [mute, setMute] = useState(false);
  const [muteFor, setMuteFor] = useState('7');
  const [reason, setReason] = useState('');
  const [unmute, setUnmute] = useState(false);

  const grouped = useMemo(
    () =>
      DEFECT_GROUPS.map((g) => ({
        group: GROUP_LABEL[g],
        items: (types.data ?? []).filter((t) => t.group === g).map((t) => ({ value: t.id, label: t.builtIn ? t.name : `${t.name} (${t.shortName})` })),
      })).filter((g) => g.items.length > 0),
    [types.data],
  );

  const issueText = issue.trim();
  const isUrl = /^https?:\/\//i.test(issueText);
  const chosen = trackers[Number(tracker)];
  const fromKey = !isUrl && issueText && chosen ? issueUrlFor(chosen.urlTemplate, chosen.keyRegex, issueText) : null;
  const issueError = issueText && !isUrl && !fromKey ? (chosen ? `Does not match ${chosen.type} keys (${chosen.keyRegex})` : 'Paste a full http(s) link; no issue tracker is configured') : null;
  const issueBody = isUrl ? { url: issueText } : fromKey ? { url: fromKey, key: issueText } : undefined;
  const reasonError = mute && !reason.trim() ? 'A mute needs a reason' : null;
  const prevIssue = target.defect?.issueKey ?? target.defect?.issueUrl ?? '';
  const changed = typeId !== (target.defect?.typeId ?? null) || comment !== (target.defect?.comment ?? '') || issueText !== prevIssue || mute || unmute;

  const submit = () => {
    const body: TriageRequest = target.resultIds?.length ? { resultIds: target.resultIds } : { launchId: target.launchId, fingerprint: target.fingerprint };
    if (typeId && typeId !== target.defect?.typeId) body.defectTypeId = typeId;
    else if (typeId && (comment !== (target.defect?.comment ?? '') || issueBody)) body.defectTypeId = typeId;
    if (comment !== (target.defect?.comment ?? '')) body.comment = comment.slice(0, MAX_COMMENT);
    if (issueBody && issueText !== prevIssue) body.issue = issueBody;
    if (mute) {
      const days = MUTE_FOR.find((m) => m.value === muteFor)?.days ?? null;
      body.mute = { reason: reason.trim(), ...(days ? { until: new Date(Date.now() + days * 86_400_000).toISOString() } : {}) };
    }
    if (unmute) body.unmute = true;
    triage.mutate(body, {
      onSuccess: (r) => {
        notifications.show({ color: 'green', title: 'Triage saved', message: `${String(r.updated)} result${r.updated === 1 ? '' : 's'} updated` });
        onDone();
      },
    });
  };

  return (
    <Stack gap="md" data-testid="triage-form">
      <Text size="sm" c="dimmed">
        {target.label}
      </Text>
      {target.defect && (
        <Group gap="xs">
          <Text size="xs" c="dimmed">
            Now:
          </Text>
          <DefectChip defect={target.defect} />
        </Group>
      )}
      <Select
        label="Defect type"
        placeholder="Pick a type"
        data={grouped}
        value={typeId}
        onChange={setTypeId}
        searchable
        comboboxProps={{ withinPortal: true }}
        data-testid="triage-type"
      />
      <Textarea label="Comment" autosize minRows={2} maxRows={8} maxLength={MAX_COMMENT} value={comment} onChange={(e) => setComment(e.currentTarget.value)} description={`${String(comment.length)} / ${String(MAX_COMMENT)}`} />
      <Stack gap={6}>
        <Group gap="xs" align="flex-end" wrap="nowrap">
          {trackers.length > 1 && !isUrl && (
            <Select
              label="Tracker"
              data={trackers.map((t, i) => ({ value: String(i), label: t.type }))}
              value={tracker}
              onChange={(v) => setTracker(v ?? '0')}
              allowDeselect={false}
              w={110}
              comboboxProps={{ withinPortal: true }}
            />
          )}
          <TextInput
            label="Issue"
            placeholder={chosen ? `Key, e.g. ${chosen.type === 'jira' ? 'SHOP-123' : '#123'}, or a link` : 'https://…'}
            value={issue}
            onChange={(e) => setIssue(e.currentTarget.value)}
            maxLength={500}
            error={issueError}
            style={{ flex: 1 }}
            data-testid="triage-issue"
          />
        </Group>
        {issueBody && (
          <Anchor href={issueBody.url} target="_blank" rel="noopener noreferrer" size="xs" data-testid="triage-issue-preview">
            {issueBody.url} <IconExternalLink size={11} />
          </Anchor>
        )}
      </Stack>
      {target.muted ? (
        <Alert variant="light" color="gray" icon={<IconBellOff size={16} />} title={target.muteUntil ? `Muted until ${formatDateTime(target.muteUntil)}` : 'Muted until unmuted'}>
          <Switch label="Unmute" checked={unmute} onChange={(e) => setUnmute(e.currentTarget.checked)} data-testid="triage-unmute" />
        </Alert>
      ) : (
        <Stack gap="xs">
          <Switch label="Mute: failures do not count against the launch" checked={mute} onChange={(e) => setMute(e.currentTarget.checked)} data-testid="triage-mute" />
          {mute && (
            <>
              <SegmentedControl size="xs" data={MUTE_FOR.map(({ value, label }) => ({ value, label }))} value={muteFor} onChange={setMuteFor} />
              <TextInput label="Reason" required maxLength={500} value={reason} onChange={(e) => setReason(e.currentTarget.value)} error={reasonError} data-testid="triage-reason" />
            </>
          )}
        </Stack>
      )}
      {triage.isError && (
        <Alert color="red" variant="light" title="Not saved">
          {errorMessage(triage.error)}
        </Alert>
      )}
      <Group justify="flex-end" gap="xs">
        <Button variant="default" onClick={onDone}>
          Cancel
        </Button>
        <Button onClick={submit} loading={triage.isPending} disabled={!changed || !!issueError || !!reasonError} data-testid="triage-save">
          Save{target.count > 1 ? ` for ${String(target.count)} results` : ''}
        </Button>
      </Group>
    </Stack>
  );
}

export function TriageDrawer({ project, target, onClose }: { project: string; target: TriageTarget | null; onClose: () => void }) {
  const phone = useMediaQuery('(max-width: 48em)');
  return (
    <Drawer opened={!!target} onClose={onClose} position="right" size={phone ? '100%' : 'md'} title={target && target.count > 1 ? `Triage ${String(target.count)} results` : 'Triage'} styles={{ title: { fontWeight: 700 } }}>
      {target && <TriageForm key={`${target.fingerprint ?? ''}:${(target.resultIds ?? []).join(',')}`} project={project} target={target} onDone={onClose} />}
    </Drawer>
  );
}
