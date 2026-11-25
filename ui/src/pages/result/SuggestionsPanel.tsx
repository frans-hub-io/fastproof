import { Card, Group, Stack, Text } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { IconBulb } from '@tabler/icons-react';
import type { ResultDetail } from '../../api/types';
import { useClusterSuggestions } from '../../api/suggestions';
import { ConfirmDecision, SuggestionCard } from '../launch/ClusterSuggestion';

/** Suggestions for the cluster of a failed result (the result's launch × fingerprint). */
export function SuggestionsPanel({ project, r }: { project: string; r: ResultDetail }) {
  const { on, cluster } = useClusterSuggestions(project, r.launchId, r.fingerprint);
  const canTriage = useCan('results:triage', project);
  const canRules = useCan('rules:write', project);
  const canPropose = useCan('ai:propose', project);
  if (!on || !r.launchId || !r.fingerprint) return null;
  const shown = (cluster?.suggestions ?? []).filter((s) => s.state === 'open' || s.state === 'accepted');
  const defect = r.defect ?? null;
  const machine = defect && ['ai', 'auto'].includes(defect.source);
  if (!shown.length && !machine) return null;
  return (
    <Card withBorder radius="md" padding="sm" data-testid="suggestions-panel">
      <Stack gap="xs">
        <Group gap={6}>
          <IconBulb size={16} />
          <Text size="sm" fw={600}>
            Suggestions
          </Text>
          {cluster && (
            <Text size="xs" c="dimmed">
              for {cluster.count} result{cluster.count === 1 ? '' : 's'} with this error
            </Text>
          )}
        </Group>
        <ConfirmDecision project={project} launchId={r.launchId} fingerprint={r.fingerprint} defect={defect} canTriage={canTriage} />
        {shown.map((s) => (
          <SuggestionCard key={s.id} project={project} s={s} cluster={cluster} canTriage={canTriage} canRule={canRules || canPropose} testId="result-suggestion" />
        ))}
      </Stack>
    </Card>
  );
}
