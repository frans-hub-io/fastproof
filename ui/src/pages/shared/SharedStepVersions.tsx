import { Badge, Button, Card, Checkbox, Group, Modal, Stack, Text } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconArrowBackUp } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { errorMessage } from '../../api/client';
import { useRestoreSharedStepVersion, useSharedStepVersion, useSharedStepVersions, type SharedStep, type SharedStepSnapshot } from '../../api/sharedSteps';
import { isConflict, type CaseSnapshot } from '../../api/tm';
import { QueryState } from '../../components/QueryState';
import { formatDateTime, formatRelative } from '../../lib/format';
import { diffSnapshots } from '../../lib/tm/versionDiff';
import { VersionDiffView } from '../cases/VersionDiffView';

const SOURCE_LABEL: Record<string, string> = { ui: 'edited', import: 'import', ai: 'AI draft', restore: 'restored', bulk: 'bulk change', code: 'code sync' };

/** A shared step's snapshot in the shape the case diff understands. */
function asCaseSnapshot(s: SharedStepSnapshot): CaseSnapshot {
  return { name: s.name, status: 'active', folderId: null, body: { description: s.description, preconditions: '', steps: s.steps, links: [] }, fields: {}, tags: [] };
}

export function SharedStepVersions({ project, s, etag, canWrite, onRestored }: { project: string; s: SharedStep; etag: string; canWrite: boolean; onRestored: () => void }) {
  const versions = useSharedStepVersions(project, s.number);
  const restore = useRestoreSharedStepVersion(project, s.number);
  const list = useMemo(() => versions.data ?? [], [versions.data]);
  const [picked, setPicked] = useState<number[]>([]);
  const pair = useMemo(() => {
    if (picked.length === 2) return [...picked].sort((a, b) => a - b);
    const latest = list[0]?.version;
    const prev = list[1]?.version;
    return latest && prev ? [prev, latest] : null;
  }, [picked, list]);
  const a = useSharedStepVersion(project, s.number, pair?.[0] ?? null);
  const b = useSharedStepVersion(project, s.number, pair?.[1] ?? null);
  const diff = a.data && b.data ? diffSnapshots(asCaseSnapshot(a.data.snapshot), asCaseSnapshot(b.data.snapshot)) : null;
  const [confirm, setConfirm] = useState<number | null>(null);
  const toggle = (v: number, on: boolean) => setPicked((p) => (on ? [...p.filter((x) => x !== v), v].slice(-2) : p.filter((x) => x !== v)));

  const doRestore = (version: number) =>
    restore.mutate(
      { version, etag },
      {
        onSuccess: (r) => {
          notifications.show({ color: 'green', title: `Version ${String(version)} restored`, message: `Saved as version ${String(r.value.version)}. Runs already started keep their steps.` });
          setConfirm(null);
          setPicked([]);
          onRestored();
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Not restored', message: isConflict(e) ? 'The shared step changed meanwhile; reload it and try again.' : errorMessage(e) }),
      },
    );

  return (
    <QueryState query={versions}>
      {() => (
        <div className="fp-tm-versions" data-testid="shared-versions">
          <Stack gap={6}>
            <Text size="xs" c="dimmed">
              Tick two versions to compare them; by default the latest two are compared.
            </Text>
            {list.map((v) => (
              <Card key={v.version} withBorder radius="md" padding="xs" data-testid="shared-version-row" data-version={v.version}>
                <Group gap="xs" wrap="nowrap" align="flex-start">
                  <Checkbox size="xs" mt={3} checked={picked.includes(v.version)} onChange={(e) => toggle(v.version, e.currentTarget.checked)} aria-label={`Compare version ${String(v.version)}`} />
                  <Stack gap={2} style={{ flex: 1, minWidth: 0 }}>
                    <Group gap={6} wrap="wrap">
                      <Text size="sm" fw={600}>
                        v{v.version}
                      </Text>
                      <Badge size="xs" variant="light" radius="sm" color={v.source === 'ai' ? 'grape' : 'gray'}>
                        {SOURCE_LABEL[v.source] ?? v.source}
                      </Badge>
                      {v.version === s.version && (
                        <Badge size="xs" variant="outline" radius="sm">
                          current
                        </Badge>
                      )}
                    </Group>
                    <Text size="xs" c="dimmed" title={formatDateTime(v.createdAt)}>
                      {v.createdBy} · {formatRelative(v.createdAt)}
                    </Text>
                    {v.changed.length > 0 && (
                      <Text size="xs" c="dimmed" className="fh-truncate">
                        {v.changed.join(', ')}
                      </Text>
                    )}
                  </Stack>
                  {canWrite && v.version !== s.version && !s.archivedAt && (
                    <Button size="compact-xs" variant="subtle" leftSection={<IconArrowBackUp size={12} />} onClick={() => setConfirm(v.version)} data-testid="shared-version-restore">
                      Restore
                    </Button>
                  )}
                </Group>
              </Card>
            ))}
          </Stack>
          <Stack gap="xs" style={{ minWidth: 0 }}>
            {pair ? (
              <>
                <Text size="sm" fw={600}>
                  v{pair[0]} → v{pair[1]}
                </Text>
                {diff ? <VersionDiffView diff={diff} beforeLabel={`v${String(pair[0])}`} afterLabel={`v${String(pair[1])}`} /> : <Text size="sm" c="dimmed">Loading…</Text>}
              </>
            ) : (
              <Text size="sm" c="dimmed">
                Only one version so far.
              </Text>
            )}
          </Stack>
          <Modal opened={confirm !== null} onClose={() => setConfirm(null)} title={`Restore version ${String(confirm ?? '')}`}>
            <Stack gap="sm">
              <Text size="sm">The shared step gets the content of version {confirm} as a new version. Cases pick it up in new runs; no history is lost.</Text>
              <Group justify="flex-end">
                <Button variant="default" onClick={() => setConfirm(null)}>
                  Cancel
                </Button>
                <Button loading={restore.isPending} onClick={() => confirm !== null && doRestore(confirm)} data-testid="shared-version-restore-confirm">
                  Restore
                </Button>
              </Group>
            </Stack>
          </Modal>
        </div>
      )}
    </QueryState>
  );
}
