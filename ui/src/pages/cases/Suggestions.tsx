import { Alert, Button, Collapse, Group, Stack, Text } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconSparkles } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { errorMessage } from '../../api/client';
import { isConflict, useCaseFields, useDecideSuggestion, type Case, type CaseSuggestion } from '../../api/tm';
import { formatRelative } from '../../lib/format';
import { currentSnapshot, diffSnapshots } from '../../lib/tm/versionDiff';
import { VersionDiffView } from './VersionDiffView';

function Suggestion({ project, c, s, etag }: { project: string; c: Case; s: CaseSuggestion; etag: string }) {
  const canWrite = useCan('cases:write', project);
  const decide = useDecideSuggestion(project, c.number);
  const fields = useCaseFields(project);
  const [open, setOpen] = useState(true);
  const labels = useMemo(() => Object.fromEntries((fields.data ?? []).map((f) => [f.key, f.name])), [fields.data]);
  const current = currentSnapshot(c);
  const proposed = { ...current, name: s.proposal.name ?? current.name, status: s.proposal.status ?? current.status, body: s.proposal.body ?? current.body, tags: s.proposal.tags ?? current.tags, fields: { ...current.fields, ...(s.proposal.fields ?? {}) } };
  const diff = diffSnapshots(current, proposed, labels);
  const stale = s.baseRevision !== c.revision;
  const act = (accept: boolean) =>
    decide.mutate(
      { id: s.id, accept, etag },
      {
        onSuccess: () => notifications.show({ color: 'green', title: accept ? 'Suggestion accepted' : 'Suggestion rejected', message: accept ? 'Saved as a new version.' : 'It stays in the history.' }),
        onError: (e) => notifications.show({ color: 'red', title: 'Not saved', message: isConflict(e) ? 'The case changed; reload it to review the suggestion again.' : errorMessage(e) }),
      },
    );
  return (
    <Alert color="grape" variant="light" radius="md" icon={<IconSparkles size={18} />} title={`Suggested by ${s.agent} ${formatRelative(s.createdAt)}`} data-testid="suggestion">
      <Stack gap="xs">
        <Text size="sm">{s.rationale}</Text>
        {stale && (
          <Text size="xs" c="orange">
            The case changed after this suggestion was made (revision {s.baseRevision} → {c.revision}); accepting it is refused.
          </Text>
        )}
        <Group gap="xs">
          <Button size="compact-xs" variant="subtle" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
            {open ? 'Hide changes' : 'Show changes'}
          </Button>
          {canWrite && (
            <>
              <Button size="compact-sm" color="grape" onClick={() => act(true)} loading={decide.isPending} disabled={stale} data-testid="suggestion-accept">
                Accept
              </Button>
              <Button size="compact-sm" variant="default" onClick={() => act(false)} loading={decide.isPending} data-testid="suggestion-reject">
                Reject
              </Button>
            </>
          )}
        </Group>
        <Collapse expanded={open}>
          <VersionDiffView diff={diff} beforeLabel="Current" afterLabel="Suggested" />
        </Collapse>
      </Stack>
    </Alert>
  );
}

export function Suggestions({ project, c, etag, items }: { project: string; c: Case; etag: string; items: CaseSuggestion[] }) {
  const pending = items.filter((s) => s.status === 'pending');
  if (!pending.length) return null;
  return (
    <Stack gap="xs" className="fp-no-print">
      {pending.map((s) => (
        <Suggestion key={s.id} project={project} c={c} s={s} etag={etag} />
      ))}
    </Stack>
  );
}
