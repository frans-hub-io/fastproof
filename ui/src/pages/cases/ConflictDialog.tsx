import { Alert, Button, Group, Modal, ScrollArea, Stack, Text } from '@mantine/core';
import { useMemo } from 'react';
import type { Case, CaseWrite, ConflictDetails } from '../../api/tm';
import { currentSnapshot, diffSnapshots, writeSnapshot } from '../../lib/tm/versionDiff';
import { VersionDiffView } from './VersionDiffView';
import { actorLabel } from '../../lib/tm/ui';

const SOURCE: Record<string, string> = { code: 'the code sync', ui: 'an edit', import: 'an import', ai: 'an accepted AI suggestion', restore: 'a restore', bulk: 'a bulk change' };

function overlapLabel(key: string, labels: Record<string, string>): string {
  if (key.startsWith('fields.')) return labels[key.slice(7)] ?? key.slice(7);
  if (key.startsWith('step ')) return 'a step';
  return { name: 'Name', status: 'Status', folderId: 'Folder', tags: 'Tags', description: 'Description', preconditions: 'Preconditions', links: 'Links' }[key] ?? key;
}

/**
 * The 412 dialog. `mine` is the user's edit already merged onto the current version (only what the user changed is
 * applied), so the diff shows exactly what saving would change.
 */
export function ConflictDialog({ conflict, current, mine, overlap = [], me, labels, onKeepMine, onTakeTheirs, onClose, busy }: { conflict: ConflictDetails; current: Case | undefined; mine: CaseWrite; overlap?: string[]; me?: string; labels: Record<string, string>; onKeepMine: () => void; onTakeTheirs: () => void; onClose: () => void; busy: boolean }) {
  const diff = useMemo(() => (current ? diffSnapshots(currentSnapshot(current), writeSnapshot(mine, current), labels) : null), [current, mine, labels]);
  const both = [...new Set(overlap.map((k) => overlapLabel(k, labels)))];
  return (
    <Modal opened onClose={onClose} title="The case changed while you were editing" size="xl" attributes={{ content: { 'data-testid': 'conflict-dialog' } }}>
      <Stack gap="sm">
        <Alert color="orange" variant="light">
          Revision {conflict.currentRevision} was saved by {actorLabel(conflict.changedBy, me)} through {SOURCE[conflict.source] ?? conflict.source}. Saving applies only the fields you changed on top of it; everything else stays as it is now.
        </Alert>
        {both.length > 0 && (
          <Alert color="red" variant="light" data-testid="conflict-overlap">
            Both of you changed {both.join(', ')}. Saving keeps your version of {both.length === 1 ? 'it' : 'them'}.
          </Alert>
        )}
        <ScrollArea.Autosize mah="55vh">
          {diff ? diff.same ? <Text size="sm">Your changes are already in the current version.</Text> : <VersionDiffView diff={diff} beforeLabel="Current version" afterLabel="After saving" /> : <Text size="sm">Loading the current version…</Text>}
        </ScrollArea.Autosize>
        <Group justify="flex-end" gap="xs" wrap="wrap">
          <Button variant="default" onClick={onClose}>
            Keep editing
          </Button>
          <Button variant="default" color="red" onClick={onTakeTheirs} data-testid="conflict-theirs">
            Discard mine, load current
          </Button>
          <Button onClick={onKeepMine} loading={busy} disabled={!current} data-testid="conflict-mine">
            Save my changes
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}
