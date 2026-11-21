import { Alert, Anchor, Button, Chip, Code, Group, Modal, Stack, Text, TextInput } from '@mantine/core';
import { IconDownload } from '@tabler/icons-react';
import { useState } from 'react';
import { errorMessage } from '../../api/client';
import { testPlanUrl, useCreateTestPlan } from '../../api/stageC';
import type { Launch, ResultStatus } from '../../api/types';
import { RESULT_STATUSES } from '../../api/types';
import { StatusDot } from '../../components/Status';
import { STATUS_LABEL } from '../../lib/status';
import { usesToken } from '../../lib/authToken';
import { downloadWithToken } from '../../viewers/authedUrl';

export function TestPlanModal({ project, launch, opened, onClose, initialStatuses }: { project: string; launch: Launch; opened: boolean; onClose: () => void; initialStatuses: ResultStatus[] }) {
  const [name, setName] = useState(`#${String(launch.number)} ${launch.name}: rerun`);
  const [statuses, setStatuses] = useState<string[]>(initialStatuses.length ? initialStatuses : ['failed', 'broken']);
  const create = useCreateTestPlan(project);
  const planId = create.data?.id;
  const selected = statuses.reduce((n, s) => n + ((RESULT_STATUSES.includes(s as ResultStatus) ? launch.counts[s as ResultStatus] : 0)), 0);
  const close = () => {
    create.reset();
    onClose();
  };
  return (
    <Modal opened={opened} onClose={close} title="Test plan from this launch" size="lg" styles={{ title: { fontWeight: 700 } }}>
      <Stack gap="md" data-testid="test-plan">
        <Text size="sm" c="dimmed">
          A test plan lists tests to run again (Allure <Code>testplan.json</Code>). Playwright, pytest and JUnit plugins that read <Code>ALLURE_TESTPLAN_PATH</Code> run only those tests.
        </Text>
        <TextInput label="Name" value={name} onChange={(e) => setName(e.currentTarget.value)} maxLength={200} required disabled={!!planId} />
        <Stack gap={6}>
          <Text size="sm" fw={500}>
            Results with status
          </Text>
          <Chip.Group multiple value={statuses} onChange={setStatuses}>
            <Group gap={6}>
              {RESULT_STATUSES.map((s) => (
                <Chip key={s} value={s} size="xs" variant="outline" disabled={!!planId}>
                  <Group gap={4} component="span" wrap="nowrap">
                    <StatusDot status={s} size={8} />
                    {STATUS_LABEL[s]} {launch.counts[s]}
                  </Group>
                </Chip>
              ))}
            </Group>
          </Chip.Group>
          <Text size="xs" c="dimmed">
            {selected} test{selected === 1 ? '' : 's'} will be in the plan.
          </Text>
        </Stack>
        {create.isError && (
          <Alert color="red" variant="light" title="Not created">
            {errorMessage(create.error)}
          </Alert>
        )}
        {planId ? (
          <Stack gap="xs">
            <Alert color="green" variant="light" title="Test plan created">
              <Stack gap={6}>
                <Anchor
                  href={testPlanUrl(project, planId)}
                  download="testplan.json"
                  data-testid="test-plan-download"
                  onClick={(e) => {
                    if (!usesToken()) return;
                    e.preventDefault();
                    void downloadWithToken(testPlanUrl(project, planId), 'testplan.json');
                  }}
                >
                  <Group gap={6} component="span">
                    <IconDownload size={14} /> Download testplan.json
                  </Group>
                </Anchor>
                <Text size="xs" c="dimmed">
                  In CI: <Code>fastproof testplan --from-launch {launch.externalId ?? launch.id} --status {statuses.join(',')} &gt; testplan.json</Code>
                </Text>
              </Stack>
            </Alert>
            <Group justify="flex-end">
              <Button variant="default" onClick={close}>
                Close
              </Button>
            </Group>
          </Stack>
        ) : (
          <Group justify="flex-end" gap="xs">
            <Button variant="default" onClick={close}>
              Cancel
            </Button>
            <Button
              onClick={() => create.mutate({ name: name.trim(), fromLaunchId: launch.id, statuses: statuses as ResultStatus[] })}
              loading={create.isPending}
              disabled={!name.trim() || statuses.length === 0 || selected === 0}
              data-testid="test-plan-create"
            >
              Create
            </Button>
          </Group>
        )}
      </Stack>
    </Modal>
  );
}
