import { Badge, Group, Stack, Text } from '@mantine/core';
import { IconPaperclip } from '@tabler/icons-react';
import { attachmentUrl } from '../../api/client';
import type { Case, CaseStep, CaseStepHtml } from '../../api/tm';
import { CaseHtml } from './CaseHtml';

interface Row {
  path: string;
  depth: number;
  step: CaseStep;
  html: CaseStepHtml | undefined;
}

function rows(steps: CaseStep[], html: CaseStepHtml[], prefix = '', depth = 0, out: Row[] = []): Row[] {
  steps.forEach((s, i) => {
    const path = prefix ? `${prefix}.${String(i + 1)}` : String(i + 1);
    const h = html.find((x) => x.id === s.id) ?? html[i];
    out.push({ path, depth, step: s, html: h });
    rows(s.steps, h?.steps ?? [], path, depth + 1, out);
  });
  return out;
}

/** The read view of a case scenario: preconditions and the steps table (action | expected | data). */
export function ScenarioView({ project, c, print = false }: { project: string; c: Case; print?: boolean }) {
  const list = rows(c.body.steps, c.html.steps);
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
        <Text fw={600} size="sm" component="h3" m={0}>
          Steps
        </Text>
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
            {list.map((r) => (
              <div role="row" key={r.step.id} className="fp-tm-steps-row" data-depth={r.depth} data-testid={print ? undefined : 'step-view'}>
                <span role="cell" className="fp-mono" style={{ paddingLeft: r.depth * 14 }}>
                  {r.path}
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
            ))}
          </div>
        )}
      </Stack>
    </Stack>
  );
}
