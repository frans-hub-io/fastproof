import { Alert, Button, Group, Modal, ScrollArea, Stack, Text } from '@mantine/core';
import { useMemo } from 'react';
import type { Case, CaseWrite, ConflictDetails } from '../../api/tm';
import { currentSnapshot, diffSnapshots, writeSnapshot } from '../../lib/tm/versionDiff';
import { VersionDiffView } from './VersionDiffView';

const SOURCE: Record<string, string> = { code: 'the code sync', ui: 'an edit', import: 'an import', ai: 'an accepted AI suggestion', restore: 'a restore', bulk: 'a bulk change' };

/** The 412 dialog: what differs between the current version and the user's edits, and what to do. */
export function ConflictDialog({ conflict, current, mine, labels, onKeepMine, onTakeTheirs, onClose, busy }: { conflict: ConflictDetails; current: Case | undefined; mine: CaseWrite; labels: Record<string, string>; onKeepMine: () => void; onTakeTheirs: () => void; onClose: () => void; busy: boolean }) {
  const diff = useMemo(() => (current ? diffSnapshots(currentSnapshot(current), writeSnapshot(mine, current), labels) : null), [current, mine, labels]);
  return (
    <Modal opened onClose={onClose} title="The case changed while you were editing" size="xl" attributes={{ content: { 'data-testid': 'conflict-dialog' } }}>
      <Stack gap="sm">
        <Alert color="orange" variant="light">
          Revision {conflict.currentRevision} was saved by {conflict.changedBy ?? 'someone'} through {SOURCE[conflict.source] ?? conflict.source}. Compare it with your changes and choose which to keep.
        </Alert>
        <ScrollArea.Autosize mah="55vh">
          {diff ? <VersionDiffView diff={diff} beforeLabel="Current version" afterLabel="Your changes" /> : <Text size="sm">Loading the current version…</Text>}
        </ScrollArea.Autosize>
        <Group justify="flex-end" gap="xs" wrap="wrap">
          <Button variant="default" onClick={onClose}>
            Keep editing
          </Button>
          <Button variant="default" color="red" onClick={onTakeTheirs} data-testid="conflict-theirs">
            Discard mine, load current
          </Button>
          <Button onClick={onKeepMine} loading={busy} disabled={!current} data-testid="conflict-mine">
            Save mine over it
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}
