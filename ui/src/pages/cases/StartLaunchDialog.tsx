import { Alert, Anchor, Autocomplete, Badge, Button, Code, Group, Modal, Select, SegmentedControl, Stack, Switch, TagsInput, Text, TextInput } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconExternalLink, IconPlayerPlay } from '@tabler/icons-react';
import { useCan } from '@franshub/ui-shell';
import { useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { useSettings } from '../../api/stageC';
import { useStartPlanLaunch, type PlanLaunch, type TestPlan } from '../../api/tm';
import { pdk, type RerunVariable } from '../../api/planDispatches';
import { CopyValue } from '../../components/CopyButton';
import { isHttpUrl } from '../../lib/format';
import { ENV_NAME, MAX_ENVIRONMENTS, ciBody } from '../../lib/tm/ci';
import { typedValueError } from '../../lib/tm/rerunPlan';


function VariableInput({ v, value, onChange }: { v: RerunVariable; value: string; onChange: (x: string) => void }) {
  const label = v.name;
  if (v.values && v.values.length > 0) {
    return (
      <Select label={label} description={v.description ?? undefined} data={v.values} value={value || null} onChange={(x) => onChange(x ?? '')} placeholder={v.default ?? 'Not set'} clearable comboboxProps={{ withinPortal: true }} data-testid={`ci-var-${v.name}`} />
    );
  }
  return <TextInput label={label} description={v.description ?? undefined} value={value} onChange={(e) => onChange(e.currentTarget.value)} placeholder={v.default ?? 'Not set'} maxLength={256} error={typedValueError(value)} data-testid={`ci-var-${v.name}`} />;
}

/** Results of the CI dispatches of a start, one row per launch. */
function DispatchResults({ project, result }: { project: string; result: PlanLaunch }) {
  const failed = result.dispatches.filter((d) => d.status === 'failed');
  return (
    <Stack gap="xs" data-testid="ci-results">
      <Text size="sm" fw={600}>
        CI
      </Text>
      {result.dispatches.map((d) => {
        const l = result.launches.find((x) => x.id === d.launchId);
        return (
          <Group key={d.id} gap="xs" wrap="wrap" data-testid="ci-result">
            <Anchor component={Link} to={`/p/${project}/launches/${d.launchId}`} size="sm" fw={600}>
              #{l?.number ?? d.launchNumber ?? ''} {d.environment ?? l?.name ?? ''}
            </Anchor>
            <Badge size="sm" variant="light" radius="sm" color={d.status === 'failed' ? 'red' : d.status === 'dispatched' ? 'green' : 'blue'}>
              {d.status}
            </Badge>
            {d.ciRef && isHttpUrl(d.ciRef) && (
              <Anchor href={d.ciRef} target="_blank" rel="noopener noreferrer" size="xs">
                <Group gap={2} component="span" wrap="nowrap">
                  Pipeline <IconExternalLink size={12} />
                </Group>
              </Anchor>
            )}
            {d.error && (
              <Text size="xs" c="red">
                {d.error}
              </Text>
            )}
          </Group>
        );
      })}
      {failed.length > 0 && (
        <Alert color="red" variant="light" data-testid="ci-failed">
          The CI was not triggered for {failed.length} of {result.dispatches.length} launches. The launches stay; fix the cause and start the plan again.
        </Alert>
      )}
    </Stack>
  );
}

export function StartLaunchDialog({ project, plan, onClose }: { project: string; plan: TestPlan; onClose: () => void }) {
  const start = useStartPlanLaunch(project, plan.id);
  const qc = useQueryClient();
  const [name, setName] = useState(`${plan.name} ${new Date().toISOString().slice(0, 10)}`);
  const [envKey, setEnvKey] = useState('staging');
  const [assign, setAssign] = useState<'plan' | 'me' | 'none'>('plan');
  const [includeAutomated, setIncludeAutomated] = useState(plan.counts.automated > 0);
  const [externalId, setExternalId] = useState(() => `plan-${plan.id.slice(0, 8)}-${String(Date.now()).slice(-6)}`);
  const [result, setResult] = useState<PlanLaunch | null>(null);
  const [manual, setManual] = useState(plan.counts.manual > 0);
  const settings = useSettings(project);
  const canCi = useCan('launches:rerun', project);
  const rerun = settings.data?.rerun ?? null;
  const ciAvailable = canCi && !!rerun && plan.counts.automated > 0;
  const [ci, setCi] = useState(false);
  const [ciEnvs, setCiEnvs] = useState<string[]>([]);
  const [ciVars, setCiVars] = useState<Record<string, string>>({});
  const [ciRef, setCiRef] = useState('');
  const useCi = ci && ciAvailable;
  const refChoices = [rerun?.ref ?? 'main', ...(rerun?.refs ?? []).filter((r) => !r.includes('*'))].filter((r, i, all) => all.indexOf(r) === i);
  const envsValid = ciEnvs.length <= MAX_ENVIRONMENTS && ciEnvs.every((e) => ENV_NAME.test(e));
  const varsValid = (rerun?.variables ?? []).every((v) => (v.values && v.values.length > 0) || !typedValueError(ciVars[v.name] ?? ''));
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
            {l.manual ? 'holds the manual cases of the plan.' : 'was started from the plan.'}
          </Text>
          {result.dispatches.length > 0 && <DispatchResults project={project} result={result} />}
          {includeAutomated && result.dispatches.length === 0 && (
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
            {
              name: name.trim(),
              ...(useCi && ciEnvs.length > 1 ? {} : { externalId }),
              environment: envKey.trim() ? { env: envKey.trim() } : undefined,
              assign,
              includeAutomated: includeAutomated || useCi,
              ...(useCi ? { includeManual: manual, ci: ciBody(ciVars, ciRef.trim(), ciEnvs) } : {}),
            },
            { onSuccess: (r) => { setResult(r); void qc.invalidateQueries({ queryKey: pdk.dispatches(project, plan.id) }); }, onError: (err) => notifications.show({ color: 'red', title: 'Launch not started', message: errorMessage(err) }) },
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
          {rerun && ciAvailable && (
            <Stack gap="xs" data-testid="ci-section">
              <Switch label="Run autotests in CI" description="Starts the CI of the project once per launch with this plan" checked={ci} onChange={(e) => setCi(e.currentTarget.checked)} data-testid="start-ci" />
              {useCi && (
                <Stack gap="xs" pl="md">
                  <TagsInput
                    label="Environments"
                    description={`One launch and one CI run per environment, at most ${String(MAX_ENVIRONMENTS)}; empty: one launch`}
                    placeholder="stage, prod"
                    value={ciEnvs}
                    onChange={(v) => setCiEnvs(v.map((x) => x.trim()).filter(Boolean))}
                    error={envsValid ? null : 'Letters, digits, . _ - and at most 64 characters each'}
                    maxTags={MAX_ENVIRONMENTS}
                    data-testid="ci-envs"
                  />
                  {(rerun.variables ?? []).map((v) => (
                    <VariableInput key={v.name} v={v} value={ciVars[v.name] ?? ''} onChange={(x) => setCiVars({ ...ciVars, [v.name]: x })} />
                  ))}
                  {(rerun.variables ?? []).length === 0 && (
                    <Text size="xs" c="dimmed">
                      The project allows no CI variables (Settings, Integrations, Rerun in CI).
                    </Text>
                  )}
                  {rerun.provider !== 'webhook' && (
                    <Autocomplete
                      label="Branch or tag"
                      description={rerun.refs && rerun.refs.length > 0 ? `Allowed: ${[rerun.ref ?? 'main', ...rerun.refs].filter((r, i, all) => all.indexOf(r) === i).join(', ')}` : `Only ${rerun.ref ?? 'main'}`}
                      placeholder={rerun.ref ?? 'main'}
                      data={refChoices}
                      value={ciRef}
                      onChange={setCiRef}
                      maxLength={200}
                      comboboxProps={{ withinPortal: true }}
                      data-testid="ci-ref"
                    />
                  )}
                  {plan.counts.manual > 0 && <Switch label={`Also create run items for the manual cases (${String(plan.counts.manual)})`} description={ciEnvs.length > 1 ? 'Each environment gets its own manual run' : undefined} checked={manual} onChange={(e) => setManual(e.currentTarget.checked)} data-testid="ci-manual" />}
                </Stack>
              )}
            </Stack>
          )}
          {includeAutomated && !useCi && (
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
          {start.isError && (
            <Alert color="red" variant="light" data-testid="start-error">
              {errorMessage(start.error)}
            </Alert>
          )}
          {plan.counts.manual === 0 && <Alert color="yellow">The plan has no manual cases; the launch only waits for CI results.</Alert>}
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" loading={start.isPending} disabled={!name.trim() || (useCi && (!envsValid || !varsValid))} leftSection={<IconPlayerPlay size={14} />} data-testid="start-submit">
              Start
            </Button>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}
