import { Stack, Text } from '@mantine/core';
import type { CaseStep, CaseStepHtml } from '../../api/tm';
import { CaseHtml } from '../cases/CaseHtml';

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

/** The steps of a shared step as a table (action | expected | data), from the server-rendered HTML. */
export function SharedStepView({ project, steps, html }: { project: string; steps: CaseStep[]; html: CaseStepHtml[] }) {
  const list = rows(steps, html);
  if (list.length === 0) {
    return (
      <Text size="sm" c="dimmed">
        No steps.
      </Text>
    );
  }
  return (
    <Stack gap={0}>
      <div className="fp-tm-steps-table" role="table" aria-label="Steps" data-testid="shared-steps-view">
        <div role="row" className="fp-tm-steps-head">
          <span role="columnheader">#</span>
          <span role="columnheader">Action</span>
          <span role="columnheader">Expected result</span>
          <span role="columnheader">Test data</span>
        </div>
        {list.map((r) => (
          <div role="row" key={r.step.id} className="fp-tm-steps-row" data-depth={r.depth} data-testid="shared-step-row">
            <span role="cell" className="fp-mono" style={{ paddingLeft: r.depth * 14 }}>
              {r.path}
            </span>
            <span role="cell">
              <CaseHtml html={r.html?.action ?? ''} project={project} />
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
    </Stack>
  );
}
