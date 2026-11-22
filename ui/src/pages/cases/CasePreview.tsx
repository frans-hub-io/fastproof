import { Anchor, Button, Card, Group, Stack, Text } from '@mantine/core';
import { IconArrowRight } from '@tabler/icons-react';
import { Link } from 'react-router';
import { useCase, type CaseStepHtml } from '../../api/tm';
import { QueryState } from '../../components/QueryState';
import { caseLabel } from '../../lib/tm/filters';
import { caseLink } from '../../lib/tm/ui';
import { CaseHtml } from './CaseHtml';
import { AiDraftBadge, AutomationIcon, CaseStatusBadge } from './common';

function Steps({ project, steps, prefix = '' }: { project: string; steps: CaseStepHtml[]; prefix?: string }) {
  return (
    <ol className="fp-tm-preview-steps">
      {steps.map((s, i) => {
        const path = prefix ? `${prefix}.${String(i + 1)}` : String(i + 1);
        return (
          <li key={s.id}>
            <CaseHtml html={s.action} project={project} />
            {s.expected && (
              <div className="fp-tm-expected">
                <Text size="xs" c="dimmed" component="span">
                  Expected:{' '}
                </Text>
                <CaseHtml html={s.expected} project={project} />
              </div>
            )}
            {s.steps.length > 0 && <Steps project={project} steps={s.steps} prefix={path} />}
          </li>
        );
      })}
    </ol>
  );
}

export function CasePreview({ project, n }: { project: string; n: number }) {
  const q = useCase(project, n);
  return (
    <Card withBorder radius="lg" padding="md" className="fp-tm-preview" data-testid="case-preview">
      <QueryState query={q} notFound="Case not found">
        {() => {
          const c = q.data?.value;
          if (!c) return null;
          return (
            <Stack gap="sm">
              <Group gap="xs" wrap="wrap">
                <AutomationIcon automated={c.automated} />
                <Text size="xs" c="dimmed" className="fp-mono">
                  {caseLabel(c.number)}
                </Text>
                <CaseStatusBadge status={c.status} size="xs" />
                {c.aiUnreviewed && <AiDraftBadge />}
              </Group>
              <Anchor component={Link} to={caseLink(project, c.number)} fw={700} size="lg" style={{ wordBreak: 'break-word' }}>
                {c.name}
              </Anchor>
              <CaseHtml html={c.html.description} project={project} />
              {c.html.preconditions && (
                <Stack gap={2}>
                  <Text size="xs" fw={600} c="dimmed">
                    Preconditions
                  </Text>
                  <CaseHtml html={c.html.preconditions} project={project} />
                </Stack>
              )}
              {c.html.steps.length > 0 ? (
                <Stack gap={2}>
                  <Text size="xs" fw={600} c="dimmed">
                    Steps
                  </Text>
                  <Steps project={project} steps={c.html.steps} />
                </Stack>
              ) : (
                <Text size="sm" c="dimmed">
                  {c.automated ? 'An automated case: its steps come from the test results.' : 'No steps yet.'}
                </Text>
              )}
              <Group justify="flex-end">
                <Button component={Link} to={caseLink(project, c.number)} size="xs" variant="light" rightSection={<IconArrowRight size={14} />}>
                  Open
                </Button>
              </Group>
            </Stack>
          );
        }}
      </QueryState>
    </Card>
  );
}
