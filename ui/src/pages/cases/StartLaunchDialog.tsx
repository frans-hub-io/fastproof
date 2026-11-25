import { Alert, Anchor, Button, Code, Group, Modal, SegmentedControl, Stack, Switch, Text, TextInput } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconPlayerPlay } from '@tabler/icons-react';
import { useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { useStartPlanLaunch, type PlanLaunch, type TestPlan } from '../../api/tm';
import { CopyValue } from '../../components/CopyButton';

export function StartLaunchDialog({ project, plan, onClose }: { project: string; plan: TestPlan; onClose: () => void }) {
  const start = useStartPlanLaunch(project, plan.id);
  const [name, setName] = useState(`${plan.name} ${new Date().toISOString().slice(0, 10)}`);
  const [envKey, setEnvKey] = useState('staging');
  const [assign, setAssign] = useState<'plan' | 'me' | 'none'>('plan');
  const [includeAutomated, setIncludeAutomated] = useState(plan.counts.automated > 0);
  const [externalId, setExternalId] = useState(() => `plan-${plan.id.slice(0, 8)}-${String(Date.now()).slice(-6)}`);
  const [result, setResult] = useState<PlanLaunch | null>(null);
  const command = `fastproof testplan --launch ${externalId} > testplan.json\nexport ALLURE_TESTPLAN_PATH="$PWD/testplan.json"`;

  if (result) {
    const l = result.launch;
    return (
      <Modal opened onClose={onClose} title="Launch started" size="lg" attributes={{ content: { 'data-testid': 'launch-started' } }}>
        <Stack gap="sm">
          <Text size="sm">
            Launch{' '}
            <Anchor component={Link} to={`/p/${project}/launches/${l.id}`}>
              #{l.number} {l.name}
            </Anchor>{' '}
            holds the manual cases of the plan.
          </Text>
          {includeAutomated && (
            <Stack gap={4}>
              <Text size="sm" fw={600}>
                Run the automated cases in CI
              </Text>
              <Text size="xs" c="dimmed">
                Allure plugins that read <Code>ALLURE_TESTPLAN_PATH</Code> run only the plan's tests; their results join this launch.
              </Text>
              <Group gap={4} wrap="nowrap" align="flex-start">
                <Code block style={{ flex: 1, minWidth: 0, whiteSpace: 'pre-wrap', wordBreak: 'break-all' }} data-testid="testplan-command">
                  {result.ciHint || command}
                </Code>
                <CopyValue value={result.ciHint || command} label="Copy the command" />
              </Group>
            </Stack>
          )}
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Close
            </Button>
            <Button component={Link} to={`/p/${project}/launches/${l.id}/run`} leftSection={<IconPlayerPlay size={14} />} data-testid="go-run">
              Start testing
            </Button>
          </Group>
        </Stack>
      </Modal>
    );
  }

  return (
    <Modal opened onClose={onClose} title={`Start a launch of ${plan.name}`} size="lg" attributes={{ content: { 'data-testid': 'start-dialog' } }}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          start.mutate(
            { name: name.trim(), externalId, environment: envKey.trim() ? { env: envKey.trim() } : undefined, assign, includeAutomated },
            { onSuccess: setResult, onError: (err) => notifications.show({ color: 'red', title: 'Launch not started', message: errorMessage(err) }) },
          );
        }}
      >
        <Stack gap="sm">
          <TextInput label="Name" value={name} onChange={(e) => setName(e.currentTarget.value)} required maxLength={200} data-testid="start-name" />
          <TextInput label="Environment" value={envKey} onChange={(e) => setEnvKey(e.currentTarget.value)} description="Shown on the launch, e.g. staging or prod-eu" />
          <SegmentedControl
            value={assign}
            onChange={(v) => setAssign(v === 'me' || v === 'none' ? v : 'plan')}
            data={[
              { value: 'plan', label: 'Assignees from the plan' },
              { value: 'me', label: 'Me' },
              { value: 'none', label: 'Nobody' },
            ]}
            aria-label="Assign the cases to"
            fullWidth
          />
          <Switch label={`Include automated cases (${String(plan.counts.automated)})`} checked={includeAutomated} onChange={(e) => setIncludeAutomated(e.currentTarget.checked)} disabled={plan.counts.automated === 0} data-testid="start-automated" />
          {includeAutomated && (
            <Stack gap={4}>
              <TextInput label="External id for CI" value={externalId} onChange={(e) => setExternalId(e.currentTarget.value.replace(/[^A-Za-z0-9._-]/g, ''))} maxLength={128} />
              <Text size="xs" c="dimmed">
                After the start, CI runs:
              </Text>
              <Code block data-testid="testplan-command-preview" style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-all' }}>
                {command}
              </Code>
            </Stack>
          )}
          {plan.counts.manual === 0 && <Alert color="yellow">The plan has no manual cases; the launch only waits for CI results.</Alert>}
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" loading={start.isPending} disabled={!name.trim()} leftSection={<IconPlayerPlay size={14} />} data-testid="start-submit">
              Start
            </Button>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}
