import { Button, Group, Modal, SegmentedControl, Stack, Text, TextInput } from '@mantine/core';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconBellOff } from '@tabler/icons-react';
import { useState } from 'react';
import { errorMessage } from '../../api/client';
import { useTriage } from '../../api/stageC';

const DAYS = [
  { value: '1', label: '1 day' },
  { value: '7', label: '7 days' },
  { value: '14', label: '14 days' },
  { value: '30', label: '30 days' },
];

/** Bytes of a mute reason, at most (as the triage API). */
const MAX_REASON = 500;

function clip(s: string): string {
  const enc = new TextEncoder();
  let out = s;
  while (enc.encode(out).length > MAX_REASON) out = out.slice(0, -1);
  return out;
}

/**
 * Mutes the series of a result for a number of days through the triage API (the existing mute
 * flow: the mute is journaled and can be undone). A hint suggests it; only a person sends it.
 */
export function MuteDialog({ project, resultId, testName, reason, onClose }: { project: string; resultId: string; testName: string; reason: string; onClose: () => void }) {
  const phone = useMediaQuery('(max-width: 48em)');
  const triage = useTriage(project);
  const [days, setDays] = useState('7');
  const [text, setText] = useState(clip(reason));
  const bytes = new TextEncoder().encode(text).length;
  const error = !text.trim() ? 'A mute needs a reason' : bytes > MAX_REASON ? `At most ${String(MAX_REASON)} bytes` : null;
  const submit = () => {
    const until = new Date(Date.now() + Number(days) * 86_400_000).toISOString();
    triage.mutate(
      { resultIds: [resultId], mute: { reason: text.trim(), until } },
      {
        onSuccess: () => {
          notifications.show({ color: 'green', title: 'Muted', message: `${testName} is muted for ${days} day${days === '1' ? '' : 's'}` });
          onClose();
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Not muted', message: errorMessage(e) }),
      },
    );
  };
  return (
    <Modal opened onClose={onClose} title="Mute with an end" fullScreen={phone} styles={{ title: { fontWeight: 700 } }}>
      <Stack gap="sm" data-testid="hint-mute-dialog">
        <Text size="sm">
          Results of <b>{testName}</b> in this environment are muted until the mute ends. The mute is journaled and can be undone from the result&apos;s triage history.
        </Text>
        <SegmentedControl value={days} onChange={setDays} data={DAYS} fullWidth data-testid="hint-mute-days" />
        <TextInput label="Reason" value={text} onChange={(e) => setText(e.currentTarget.value)} error={error} data-testid="hint-mute-reason" />
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
          <Button leftSection={<IconBellOff size={14} />} onClick={submit} loading={triage.isPending} disabled={!!error} data-testid="hint-mute-submit">
            Mute
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}
