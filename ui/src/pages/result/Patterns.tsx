import { Anchor, Badge, Button, Card, Code, Group, Loader, Stack, Text } from '@mantine/core';
import { useQuery } from '@tanstack/react-query';
import { useState } from 'react';
import { Link } from 'react-router';
import { projectPath, request, withQuery } from '../../api/client';
import type { Page, ResultDetail, ResultSummary } from '../../api/types';
import { StatusDot } from '../../components/Status';

type Mark = NonNullable<ResultDetail['patterns']>[number];

/** Results of the launch marked with a pattern, at most 50. */
function useMarked(project: string, launchId: string | undefined, pattern: string, enabled: boolean) {
  return useQuery({
    queryKey: ['patternResults', project, launchId, pattern],
    queryFn: () => request<Page<ResultSummary>>(withQuery(`${projectPath(project)}/launches/${encodeURIComponent(launchId ?? '')}/results`, { pattern, limit: 50 })),
    enabled: enabled && !!launchId,
  });
}

function MarkCard({ project, r, m }: { project: string; r: ResultDetail; m: Mark }) {
  const [open, setOpen] = useState(false);
  const q = useMarked(project, r.launchId, m.pattern, open);
  const others = (q.data?.items ?? []).filter((x) => x.id !== r.id);
  return (
    <Card withBorder radius="md" padding="sm" data-testid="result-pattern">
      <Stack gap={6}>
        <Group gap="xs" wrap="wrap">
          <Badge variant="light" color="fhPurple" className="fp-mono" style={{ textTransform: 'none' }}>
            {m.pattern}
          </Badge>
          <Text size="xs" c="dimmed">
            attempt {m.attempt + 1} ·{' '}
            <Anchor component={Link} to={`/p/${project}/rules`} size="xs">
              rule
            </Anchor>
          </Text>
        </Group>
        {m.excerpt && (
          <Code block style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }} data-testid="result-pattern-excerpt">
            {m.excerpt}
          </Code>
        )}
        {r.launchId && (
          <Group gap="xs">
            <Button size="compact-xs" variant="subtle" onClick={() => setOpen((v) => !v)} data-testid="result-pattern-others">
              {open ? 'Hide other results' : 'Other results with this pattern in the launch'}
            </Button>
            {open && q.isFetching && <Loader size="xs" />}
          </Group>
        )}
        {open && q.isSuccess && (
          <Stack gap={2}>
            {others.length === 0 ? (
              <Text size="xs" c="dimmed">
                No other result of this launch has it.
              </Text>
            ) : (
              others.map((x) => (
                <Anchor key={x.id} component={Link} to={`/p/${project}/results/${x.id}`} size="xs" c="inherit" data-testid="result-pattern-other">
                  <Group gap={6} wrap="nowrap">
                    <StatusDot status={x.status} size={8} />
                    <span className="fh-truncate">{x.fullName}</span>
                  </Group>
                </Anchor>
              ))
            )}
            {q.data.next && (
              <Text size="xs" c="dimmed">
                and more
              </Text>
            )}
          </Stack>
        )}
      </Stack>
    </Card>
  );
}

/** Log pattern marks of a result: set by rules with `action.pattern` after the launch. */
export function Patterns({ project, r }: { project: string; r: ResultDetail }) {
  const marks = r.patterns ?? [];
  if (marks.length === 0) return null;
  return (
    <Stack gap="xs" data-testid="result-patterns">
      <Text size="sm" fw={600}>
        Log patterns
      </Text>
      <Text size="xs" c="dimmed">
        Rules that read the logs of failed attempts marked this result after the launch finished. The line is shown as it was logged (control characters removed).
      </Text>
      {marks.map((m) => (
        <MarkCard key={m.pattern} project={project} r={r} m={m} />
      ))}
    </Stack>
  );
}
