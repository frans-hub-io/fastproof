import { ActionIcon, Anchor, Badge, Button, Checkbox, Group, Modal, Stack, Text, Textarea, TextInput } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconChevronDown, IconChevronRight, IconLink, IconPaperclip, IconScissors } from '@tabler/icons-react';
import { useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import { Link } from 'react-router';
import { attachmentUrl, errorMessage } from '../../api/client';
import { sharedKey, useExtractSharedStep, type SharedStepBrief } from '../../api/sharedSteps';
import { etagOf, tmk, type Case, type CaseStep, type CaseStepHtml, type Versioned } from '../../api/tm';
import { adjacentRun } from '../../lib/tm/shared';
import { CaseHtml } from './CaseHtml';

interface Row {
  path: string;
  depth: number;
  step: CaseStep;
  html: CaseStepHtml | undefined;
  /** A reference to a shared step: the step at its runnable version, when the server sent it. */
  shared?: { number: number; brief: SharedStepBrief | undefined };
}

function rows(steps: CaseStep[], html: CaseStepHtml[], briefs: SharedStepBrief[], expanded: (id: string) => boolean, prefix = '', depth = 0, out: Row[] = []): Row[] {
  steps.forEach((s, i) => {
    const path = prefix ? `${prefix}.${String(i + 1)}` : String(i + 1);
    if (s.shared) {
      const brief = briefs.find((b) => b.id === s.shared?.id);
      out.push({ path, depth, step: s, html: undefined, shared: { number: s.shared.number, brief } });
      if (brief && expanded(s.id)) rows(brief.steps, brief.stepsHtml, briefs, expanded, path, depth + 1, out);
      return;
    }
    const h = html.find((x) => x.id === s.id) ?? html[i];
    out.push({ path, depth, step: s, html: h });
    rows(s.steps, h?.steps ?? [], briefs, expanded, path, depth + 1, out);
  });
  return out;
}

function ExtractDialog({ project, c, stepIds, onClose, onDone }: { project: string; c: Case; stepIds: string[]; onClose: () => void; onDone: () => void }) {
  const qc = useQueryClient();
  const extract = useExtractSharedStep(project, c.number);
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const etag = qc.getQueryData<Versioned<Case>>(tmk.case(project, c.number))?.etag ?? etagOf(c.revision);
  return (
    <Modal opened onClose={onClose} title="Extract into a shared step">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          extract.mutate(
            { body: { stepIds, name: name.trim(), description: description || undefined }, etag },
            {
              onSuccess: (r) => {
                notifications.show({ color: 'green', title: `${sharedKey(r.sharedStep.number)} created`, message: 'The steps now live in the shared step; this case references it.' });
                onDone();
              },
              onError: (err) => notifications.show({ color: 'red', title: 'Not extracted', message: errorMessage(err) }),
            },
          );
        }}
      >
        <Stack gap="sm">
          <Text size="sm">
            {stepIds.length} adjacent {stepIds.length === 1 ? 'step moves' : 'steps move'} into a new shared step and this case references it, as one new version of the case.
          </Text>
          <TextInput label="Name" value={name} onChange={(e) => setName(e.currentTarget.value)} required maxLength={200} data-autofocus data-testid="extract-name" />
          <Textarea label="Description" value={description} onChange={(e) => setDescription(e.currentTarget.value)} autosize minRows={2} maxLength={4000} />
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={!name.trim()} loading={extract.isPending} data-testid="extract-confirm">
              Extract
            </Button>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}

/** The read view of a case scenario: preconditions and the steps table (action | expected | data). */
export function ScenarioView({ project, c, print = false }: { project: string; c: Case; print?: boolean }) {
  const canWrite = useCan('cases:write', project);
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  const [selecting, setSelecting] = useState(false);
  const [picked, setPicked] = useState<ReadonlySet<string>>(new Set());
  const [extracting, setExtracting] = useState<string[] | null>(null);
  const list = rows(c.body.steps, c.html.steps, c.sharedSteps, (id) => print || open.has(id));
  const hasShared = c.body.steps.some((x) => x.shared);
  const run = adjacentRun(c.body.steps, picked);
  const canExtract = !print && canWrite && !c.automated && c.archivedAt === null && c.body.steps.length > 0;
  const toggleOpen = (id: string) => setOpen((o) => new Set(o.has(id) ? [...o].filter((x) => x !== id) : [...o, id]));
  const togglePick = (id: string, on: boolean) => setPicked((p) => new Set(on ? [...p, id] : [...p].filter((x) => x !== id)));
  return (
    <Stack gap="md" data-testid={print ? undefined : 'scenario-view'}>
      <CaseHtml html={c.html.description} project={project} />
      <Stack gap={4}>
        <Text fw={600} size="sm" component="h3" m={0}>
          Preconditions
        </Text>
        <CaseHtml html={c.html.preconditions} project={project} empty={<Text size="sm" c="dimmed">None</Text>} />
      </Stack>
      <Stack gap={4}>
        <Group justify="space-between" gap="xs">
          <Text fw={600} size="sm" component="h3" m={0}>
            Steps
          </Text>
          {canExtract && (
            <Group gap="xs" className="fp-no-print">
              {selecting && (
                <>
                  <Text size="xs" c="dimmed">
                    {picked.size === 0 ? 'Tick adjacent steps' : run ? `${String(run.length)} selected` : 'The steps must be adjacent and not shared'}
                  </Text>
                  <Button size="compact-xs" disabled={!run} onClick={() => setExtracting(run)} data-testid="extract-start">
                    Extract…
                  </Button>
                </>
              )}
              <Button
                size="compact-xs"
                variant="subtle"
                leftSection={<IconScissors size={12} />}
                onClick={() => {
                  setSelecting((v) => !v);
                  setPicked(new Set());
                }}
                data-testid="extract-toggle"
              >
                {selecting ? 'Cancel' : 'Extract to shared step'}
              </Button>
            </Group>
          )}
        </Group>
        {list.length === 0 ? (
          <Text size="sm" c="dimmed">
            {c.automated ? 'An automated case: its steps come from the test results.' : 'No steps yet.'}
          </Text>
        ) : (
          <div className="fp-tm-steps-table" role="table" aria-label="Steps">
            <div role="row" className="fp-tm-steps-head">
              <span role="columnheader">#</span>
              <span role="columnheader">Action</span>
              <span role="columnheader">Expected result</span>
              <span role="columnheader">Test data</span>
            </div>
            {list.map((r) =>
              r.shared ? (
                <div role="row" key={r.step.id} className="fp-tm-steps-row fp-tm-shared-row" data-depth={r.depth} data-testid={print ? undefined : 'step-shared-view'}>
                  <span role="cell" className="fp-mono" style={{ paddingLeft: r.depth * 14 }}>
                    {r.path}
                  </span>
                  <span role="cell" style={{ gridColumn: '2 / -1' }}>
                    <Group gap={6} wrap="wrap">
                      {!print && (
                        <ActionIcon size="sm" variant="subtle" aria-expanded={open.has(r.step.id)} aria-label={`${open.has(r.step.id) ? 'Hide' : 'Show'} the steps of ${sharedKey(r.shared.number)}`} onClick={() => toggleOpen(r.step.id)} data-testid="shared-toggle" disabled={!r.shared.brief}>
                          {open.has(r.step.id) ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
                        </ActionIcon>
                      )}
                      <Badge size="sm" variant="outline" radius="sm" className="fp-mono" leftSection={<IconLink size={10} />}>
                        {sharedKey(r.shared.number)}
                      </Badge>
                      <Anchor component={Link} to={`/p/${project}/shared-steps/${String(r.shared.number)}`} size="sm" fw={500}>
                        {r.shared.brief?.name ?? 'Shared step'}
                      </Anchor>
                      {r.shared.brief && (
                        <Text size="xs" c="dimmed">
                          v{r.shared.brief.version} · {r.shared.brief.steps.length} {r.shared.brief.steps.length === 1 ? 'step' : 'steps'}
                          {r.shared.brief.archived ? ' · archived' : ''}
                        </Text>
                      )}
                    </Group>
                  </span>
                </div>
              ) : (
                <div role="row" key={`${r.path}-${r.step.id}`} className="fp-tm-steps-row" data-depth={r.depth} data-testid={print ? undefined : 'step-view'}>
                  <span role="cell" className="fp-mono" style={{ paddingLeft: r.depth * 14 }}>
                    {selecting && r.depth === 0 && !hasSharedAt(c, r.step.id) ? (
                      <Checkbox size="xs" checked={picked.has(r.step.id)} onChange={(e) => togglePick(r.step.id, e.currentTarget.checked)} aria-label={`Select step ${r.path}`} data-testid="extract-pick" label={r.path} />
                    ) : (
                      r.path
                    )}
                  </span>
                  <span role="cell">
                    <CaseHtml html={r.html?.action ?? ''} project={project} />
                    {r.step.attachments.length > 0 && (
                      <Group gap={4} wrap="wrap" mt={4}>
                        {r.step.attachments.map((id) => (
                          <Badge key={id} component="a" href={attachmentUrl(project, id)} target="_blank" rel="noopener noreferrer" size="xs" variant="outline" color="gray" radius="sm" leftSection={<IconPaperclip size={10} />} style={{ cursor: 'pointer' }}>
                            {id.slice(0, 8)}
                          </Badge>
                        ))}
                      </Group>
                    )}
                  </span>
                  <span role="cell">
                    <span className="fp-tm-cell-label">Expected: </span>
                    <CaseHtml html={r.html?.expected ?? ''} project={project} empty="—" />
                  </span>
                  <span role="cell">
                    <span className="fp-tm-cell-label">Data: </span>
                    <CaseHtml html={r.html?.data ?? ''} project={project} empty="—" />
                  </span>
                </div>
              ),
            )}
          </div>
        )}
        {hasShared && !print && (
          <Text size="xs" c="dimmed">
            Shared steps show their current version; a run pins the version it starts with.
          </Text>
        )}
      </Stack>
      {extracting && (
        <ExtractDialog
          project={project}
          c={c}
          stepIds={extracting}
          onClose={() => setExtracting(null)}
          onDone={() => {
            setExtracting(null);
            setSelecting(false);
            setPicked(new Set());
          }}
        />
      )}
    </Stack>
  );
}

function hasSharedAt(c: Case, id: string): boolean {
  return c.body.steps.some((s) => s.id === id && s.shared);
}
