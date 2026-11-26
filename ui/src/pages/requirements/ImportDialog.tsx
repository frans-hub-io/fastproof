import { Alert, Button, Group, List, Loader, Modal, NumberInput, Select, Stack, Text, TextInput, Textarea } from '@mantine/core';
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useState } from 'react';
import { errorMessage } from '../../api/client';
import { applyRequirementImport, requirementError, rqk, startRequirementImport } from '../../api/requirements';
import { useSettings } from '../../api/stageC';
import { useTmJob, type TmJob } from '../../api/tm';
import { connectedTrackers } from '../../lib/trackers';

const QUERY_HINT: Record<string, string> = {
  jira: 'A JQL query, for example: project = SHOP AND issuetype = Story',
  github: 'A label, for example: requirement',
  gitlab: 'A label, for example: requirement',
  azure: 'A WIQL query, for example: SELECT [System.Id] FROM WorkItems WHERE [System.WorkItemType] = \'User Story\'',
};

function failure(job: TmJob): string {
  const first = job.report?.errors[0];
  return first?.message ?? 'The import failed.';
}

export function ImportDialog({ project, onClose }: { project: string; onClose: () => void }) {
  const qc = useQueryClient();
  const settings = useSettings(project);
  const trackers = connectedTrackers(settings.data?.issueTrackers);
  const [trackerId, setTrackerId] = useState('');
  const [query, setQuery] = useState('');
  const [parent, setParent] = useState<number | string>('');
  const [jobId, setJobId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const job = useTmJob(project, jobId);
  const j = job.data;
  const chosen = trackers.find((t) => t.id === trackerId);
  const effectiveTracker = trackerId || (trackers.length === 1 ? (trackers[0]?.id ?? '') : '');
  const running = !!jobId && (!j || ['queued', 'dry_run', 'running'].includes(j.status));
  const locked = !!jobId && j?.status !== 'failed';
  const canStart = !!effectiveTracker && !!query.trim() && !starting && !running;

  const run = (fn: () => Promise<TmJob>) => {
    setError(null);
    setStarting(true);
    fn()
      .then((started) => {
        setJobId(started.id);
        if (started.status === 'done') void qc.invalidateQueries({ queryKey: rqk.all(project) });
      })
      .catch((e: unknown) => setError(requirementError(e)))
      .finally(() => setStarting(false));
  };

  const start = () => run(() => startRequirementImport(project, { trackerId: effectiveTracker, query: query.trim(), parentNumber: parent === '' ? null : Number(parent) }));
  const apply = () => {
    if (!jobId) return;
    run(() => applyRequirementImport(project, jobId));
  };
  const finished = j?.status === 'done';
  useEffect(() => {
    if (finished) void qc.invalidateQueries({ queryKey: rqk.all(project) });
  }, [finished, qc, project]);
  const isReport = j?.kind === 'requirements' && j.report;
  const applied = j?.status === 'done';

  return (
    <Modal opened onClose={onClose} title="Import requirements from a tracker" size="lg">
      <Stack gap="sm" data-testid="import-dialog">
        {trackers.length > 0 ? (
          <Select label="Tracker" allowDeselect={false} data={trackers.map((t) => ({ value: t.id, label: `${t.id} (${t.type})` }))} value={effectiveTracker || null} onChange={(v) => setTrackerId(v ?? '')} disabled={locked} data-testid="import-tracker" />
        ) : (
          <TextInput label="Tracker id" description="The id of an issue tracker with an API connection in the project settings." value={trackerId} onChange={(e) => setTrackerId(e.currentTarget.value)} disabled={locked} data-testid="import-tracker" />
        )}
        <Textarea label="Query" description={QUERY_HINT[chosen?.type ?? trackers[0]?.type ?? ''] ?? 'JQL for Jira, a label for GitLab and GitHub, WIQL for Azure.'} autosize minRows={2} value={query} onChange={(e) => setQuery(e.currentTarget.value)} disabled={locked} data-testid="import-query" />
        <NumberInput label="Parent requirement number" description="Imported requirements go under it. Leave empty for the top level." min={1} allowDecimal={false} hideControls value={parent} onChange={setParent} disabled={locked} />
        {error && <Alert color="red">{error}</Alert>}
        {job.isError && <Alert color="red">{errorMessage(job.error)}</Alert>}
        {running && (
          <Group gap="xs" data-testid="import-running">
            <Loader size="xs" />
            <Text size="sm">{j?.status === 'running' ? 'Working…' : 'Waiting for the server…'}</Text>
          </Group>
        )}
        {j?.status === 'failed' && (
          <Alert color="red" title="Import failed" data-testid="import-failed">
            {failure(j)}
          </Alert>
        )}
        {isReport && (j.status === 'ready' || applied) && (
          <Alert color={applied ? 'green' : 'blue'} title={applied ? 'Imported' : 'Dry run done: nothing is changed yet'} data-testid="import-report">
            <Text size="sm">
              {applied ? 'Created' : 'Would create'} <b>{j.report?.create ?? 0}</b>, {applied ? 'updated' : 'would update'} <b>{j.report?.update ?? 0}</b>, skipped <b>{j.report?.skip ?? 0}</b>.
            </Text>
            {(j.report?.errors.length ?? 0) > 0 && (
              <List size="xs" mt={6}>
                {j.report?.errors.slice(0, 5).map((e, i) => (
                  <List.Item key={i}>{e.message}</List.Item>
                ))}
              </List>
            )}
          </Alert>
        )}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            {applied ? 'Close' : 'Cancel'}
          </Button>
          {!jobId || j?.status === 'failed' ? (
            <Button
              onClick={() => {
                setJobId(null);
                start();
              }}
              disabled={!canStart}
              loading={starting}
              data-testid="import-start"
            >
              Check the import
            </Button>
          ) : (
            j?.status === 'ready' && (
              <Button onClick={apply} loading={starting} data-testid="import-apply">
                Apply
              </Button>
            )
          )}
        </Group>
      </Stack>
    </Modal>
  );
}
