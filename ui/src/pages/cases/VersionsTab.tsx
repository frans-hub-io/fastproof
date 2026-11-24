import { Anchor, Badge, Button, Card, Checkbox, Group, Modal, Stack, Text } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconArrowBackUp } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { sharedKey, useCaseSharedStepChanges } from '../../api/sharedSteps';
import { isConflict, useCaseFields, useCaseFolders, useCaseVersion, useCaseVersions, useRestoreCaseVersion, type Case } from '../../api/tm';
import { QueryState } from '../../components/QueryState';
import { formatDateTime, formatRelative } from '../../lib/format';
import { folderPathLabel } from '../../lib/tm/ui';
import { diffSnapshots } from '../../lib/tm/versionDiff';
import { VersionDiffView } from './VersionDiffView';

const SOURCE_LABEL: Record<string, string> = { ui: 'edited', code: 'code sync', import: 'import', ai: 'AI suggestion', restore: 'restored', bulk: 'bulk change' };

/** The versions written to the shared steps the case uses: they change what the case does without a case version. */
function SharedStepChanges({ project, caseNumber }: { project: string; caseNumber: number }) {
  const changes = useCaseSharedStepChanges(project, caseNumber);
  const items = changes.data ?? [];
  if (items.length === 0) return null;
  return (
    <Stack gap={6} mt="md" component="section" aria-labelledby="shared-changes-title" data-testid="shared-step-changes">
      <Text size="sm" fw={600} component="h3" m={0} id="shared-changes-title">
        Shared step changes
      </Text>
      <Text size="xs" c="dimmed">
        Edits of the shared steps this case uses. They do not create case versions; runs started after an edit use the new steps.
      </Text>
      {items.map((x) => (
        <Card key={`${String(x.sharedStepNumber)}-${String(x.version)}`} withBorder radius="md" padding="xs" data-testid="shared-step-change">
          <Group gap={6} wrap="wrap">
            <Anchor component={Link} to={`/p/${project}/shared-steps/${String(x.sharedStepNumber)}?tab=versions`} size="sm" fw={600}>
              {sharedKey(x.sharedStepNumber)} v{x.version}
            </Anchor>
            <Text size="sm" className="fh-truncate" style={{ minWidth: 0 }}>
              {x.name}
            </Text>
            <Badge size="xs" variant="light" radius="sm" color={x.source === 'ai' ? 'grape' : 'gray'}>
              {SOURCE_LABEL[x.source] ?? x.source}
            </Badge>
          </Group>
          <Text size="xs" c="dimmed" title={formatDateTime(x.createdAt)}>
            {x.createdBy} · {formatRelative(x.createdAt)}
            {x.changed.length ? ` · ${x.changed.join(', ')}` : ''}
          </Text>
        </Card>
      ))}
    </Stack>
  );
}

export function VersionsTab({ project, c, etag, onRestored }: { project: string; c: Case; etag: string; onRestored: () => void }) {
  const canWrite = useCan('cases:write', project);
  const versions = useCaseVersions(project, c.number);
  const fields = useCaseFields(project);
  const folders = useCaseFolders(project);
  const restore = useRestoreCaseVersion(project, c.number);
  const list = useMemo(() => versions.data ?? [], [versions.data]);
  const [picked, setPicked] = useState<number[]>([]);
  const pair = useMemo(() => {
    if (picked.length === 2) return [...picked].sort((a, b) => a - b);
    const latest = list[0]?.version;
    const prev = list[1]?.version;
    return latest && prev ? [prev, latest] : null;
  }, [picked, list]);
  const a = useCaseVersion(project, c.number, pair?.[0] ?? null);
  const b = useCaseVersion(project, c.number, pair?.[1] ?? null);
  const labels = useMemo(() => Object.fromEntries((fields.data ?? []).map((f) => [f.key, f.name])), [fields.data]);
  const diff = a.data && b.data ? diffSnapshots(a.data.snapshot, b.data.snapshot, labels, (id) => folderPathLabel(folders.data, id)) : null;
  const [confirm, setConfirm] = useState<number | null>(null);

  const toggle = (v: number, on: boolean) => setPicked((p) => (on ? [...p.filter((x) => x !== v), v].slice(-2) : p.filter((x) => x !== v)));

  const doRestore = (version: number) =>
    restore.mutate(
      { version, etag },
      {
        onSuccess: (r) => {
          notifications.show({ color: 'green', title: `Version ${String(version)} restored`, message: `Saved as version ${String(r.value.version)}` });
          setConfirm(null);
          setPicked([]);
          onRestored();
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Not restored', message: isConflict(e) ? 'The case changed meanwhile; reload it and try again.' : errorMessage(e) }),
      },
    );

  return (
    <QueryState query={versions}>
      {() => (
        <div className="fp-tm-versions" data-testid="versions-tab">
          <Stack gap={6}>
            <Text size="xs" c="dimmed">
              Tick two versions to compare them; by default the latest two are compared.
            </Text>
            {list.map((v) => (
              <Card key={v.version} withBorder radius="md" padding="xs" data-testid="version-row" data-version={v.version}>
                <Group gap="xs" wrap="nowrap" align="flex-start">
                  <Checkbox size="xs" mt={3} checked={picked.includes(v.version)} onChange={(e) => toggle(v.version, e.currentTarget.checked)} aria-label={`Compare version ${String(v.version)}`} />
                  <Stack gap={2} style={{ flex: 1, minWidth: 0 }}>
                    <Group gap={6} wrap="wrap">
                      <Text size="sm" fw={600}>
                        v{v.version}
                      </Text>
                      <Badge size="xs" variant="light" radius="sm" color={v.source === 'code' ? 'blue' : v.source === 'ai' ? 'grape' : 'gray'}>
                        {SOURCE_LABEL[v.source] ?? v.source}
                      </Badge>
                      {v.version === c.version && (
                        <Badge size="xs" variant="outline" radius="sm">
                          current
                        </Badge>
                      )}
                    </Group>
                    <Text size="xs" c="dimmed" title={formatDateTime(v.createdAt)}>
                      {v.createdBy} · {formatRelative(v.createdAt)}
                      {v.launchId ? (
                        <>
                          {' · '}
                          <Link to={`/p/${project}/launches/${v.launchId}`}>launch</Link>
                        </>
                      ) : null}
                    </Text>
                    {v.changed.length > 0 && (
                      <Text size="xs" c="dimmed" className="fh-truncate">
                        {v.changed.join(', ')}
                      </Text>
                    )}
                  </Stack>
                  {canWrite && v.version !== c.version && (
                    <Button size="compact-xs" variant="subtle" leftSection={<IconArrowBackUp size={12} />} onClick={() => setConfirm(v.version)} data-testid="version-restore">
                      Restore
                    </Button>
                  )}
                </Group>
              </Card>
            ))}
            <SharedStepChanges project={project} caseNumber={c.number} />
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
              <Text size="sm">The case gets the content of version {confirm} as a new version; no history is lost.</Text>
              <Group justify="flex-end">
                <Button variant="default" onClick={() => setConfirm(null)}>
                  Cancel
                </Button>
                <Button loading={restore.isPending} onClick={() => confirm !== null && doRestore(confirm)} data-testid="version-restore-confirm">
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
