import { Alert, Text } from '@mantine/core';
import { IconAlertTriangle } from '@tabler/icons-react';
import { useLaunch } from '../../api/hooks';
import { useLaunchPlanCheck } from '../../api/planDispatches';

/** What the finished launch ran compared to its plan: a banner on a warning, a quiet line for cases without a result. */
export function PlanCheck({ projectKey, launchId }: { projectKey: string; launchId: string }) {
  const launch = useLaunch(projectKey, launchId);
  const q = useLaunchPlanCheck(projectKey, launchId, !!launch.data?.testPlanId && launch.data.status !== 'running');
  const c = q.data;
  if (!c) return null;
  if (!c.warning && c.missing === 0) return null;
  if (!c.warning) {
    return (
      <Text size="xs" c="dimmed" mb="xs" data-testid="plan-check-note">
        {c.missing} of {c.inPlan} automated cases of the plan have no result in this launch.
      </Text>
    );
  }
  return (
    <Alert color="yellow" variant="light" icon={<IconAlertTriangle size={16} />} title="This launch does not match its plan" mb="sm" data-testid="plan-check">
      <Text size="sm">
        {c.ran === 0 && c.inPlan > 0 ? `None of the ${String(c.inPlan)} automated cases of the plan ran.` : `${String(c.ran)} of ${String(c.inPlan)} automated cases of the plan ran (${String(c.missing)} without a result).`}
        {c.extra > 0 && ` ${String(c.extra)} result${c.extra === 1 ? ' is' : 's are'} not linked to a case of the plan.`}
      </Text>
      {c.extraNames.length > 0 && (
        <Text size="xs" c="dimmed" mt={4} style={{ wordBreak: 'break-word' }} data-testid="plan-check-extra">
          Not in the plan: {c.extraNames.join(', ')}
          {c.extra > c.extraNames.length ? ` and ${String(c.extra - c.extraNames.length)} more` : ''}
        </Text>
      )}
    </Alert>
  );
}
