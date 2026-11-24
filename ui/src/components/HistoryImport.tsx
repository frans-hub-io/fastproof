import { Alert, Badge, Button, Card, FileInput, Group, List, SegmentedControl, SimpleGrid, Stack, Text, TextInput, Title } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconHistory } from '@tabler/icons-react';
import { useEffect, useRef, useState } from 'react';
import { errorMessage } from '../api/client';
import { isActive, planFiles, useCreateHistoryImport, useHistoryImports, useRefreshAfterImport, type HistoryImport as Import, type HistorySource } from '../api/historyImports';
import { formatDateTime } from '../lib/format';
import { QueryState } from './QueryState';

const STATUS_COLOR: Record<string, string> = { queued: 'gray', running: 'blue', done: 'green', failed: 'red' };

function Counts({ i }: { i: Import }) {
  return (
    <Text size="xs" c="dimmed">
      {i.launchesCreated} launches, {i.resultsImported} results stored
      {i.resultsExisting > 0 ? `, ${String(i.resultsExisting)} already there` : ''}
      {i.resultsUnmatched > 0 ? `, ${String(i.resultsUnmatched)} of unknown tests` : ''}
      {i.resultsSkipped > 0 ? `, ${String(i.resultsSkipped)} left out` : ''}
    </Text>
  );
}

function ImportRow({ i }: { i: Import }) {
  return (
    <Stack gap={4} py="xs" data-testid="history-import-row" style={{ borderTop: '1px solid var(--mantine-color-default-border)' }}>
      <Group gap="xs" wrap="wrap">
        <Badge color={STATUS_COLOR[i.status] ?? 'gray'} variant="light" style={{ textTransform: 'none' }} data-testid="history-import-status">
          {i.status}
        </Badge>
        <Text size="sm" fw={500}>
          {i.source === 'allure3' ? 'Allure 3' : 'Allure 2'}
        </Text>
        <Text size="sm" c="dimmed">
          {[i.branch, i.environment].filter(Boolean).join(' · ') || 'no branch or environment'}
        </Text>
        <Text size="xs" c="dimmed" ml="auto">
          {formatDateTime(i.createdAt)} · {i.createdBy}
        </Text>
      </Group>
      <Text size="sm">
        {i.pointsDone} of {i.pointsTotal} reports
      </Text>
      <Counts i={i} />
      {i.error && (
        <Text size="xs" c="red" data-testid="history-import-error">
          {i.error}
        </Text>
      )}
      {i.warnings.length > 0 && (
        <List size="xs" c="dimmed" data-testid="history-import-warnings">
          {i.warnings.map((w) => (
            <List.Item key={w}>{w}</List.Item>
          ))}
        </List>
      )}
    </Stack>
  );
}

export function HistoryImport({ project, canWrite }: { project: string; canWrite: boolean }) {
  const imports = useHistoryImports(project);
  const create = useCreateHistoryImport(project);
  const refresh = useRefreshAfterImport(project);
  const [source, setSource] = useState<HistorySource>('allure2');
  const [files, setFiles] = useState<File[]>([]);
  const [branch, setBranch] = useState('');
  const [environment, setEnvironment] = useState('');
  const [problem, setProblem] = useState<string | null>(null);
  const active = imports.data?.some(isActive) ?? false;
  const wasActive = useRef(false);
  useEffect(() => {
    if (wasActive.current && !active) refresh();
    wasActive.current = active;
  });

  const submit = () => {
    const plan = planFiles(source, files);
    if ('error' in plan) {
      setProblem(plan.error);
      return;
    }
    setProblem(null);
    create.mutate(
      { source, branch, environment, form: plan.form },
      {
        onSuccess: () => {
          setFiles([]);
          notifications.show({ color: 'green', message: 'Import queued' });
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Import not started', message: errorMessage(e) }),
      },
    );
  };

  return (
    <Stack gap="md">
      <Card withBorder radius="md" padding="md">
        <Stack gap="sm">
          <div>
            <Title order={5}>Import Allure history</Title>
            <Text size="sm" c="dimmed">
              Bring in the history of earlier Allure reports as past launches, joined to the same tests your live results use, so flaky detection, trends, dashboards and new failures see what ran before FastProof.
              Import after the first live run: Allure 2 history files name no tests, so only tests the project knows are joined.
            </Text>
          </div>
          <SegmentedControl
            value={source}
            onChange={(v) => {
              setSource(v === 'allure3' ? 'allure3' : 'allure2');
              setFiles([]);
              setProblem(null);
            }}
            data={[
              { value: 'allure2', label: 'Allure 2' },
              { value: 'allure3', label: 'Allure 3' },
            ]}
            disabled={!canWrite}
            data-testid="history-source"
          />
          <FileInput
            label={source === 'allure2' ? 'history.json and the trend files' : 'History file (one JSON line per report)'}
            description={
              source === 'allure2'
                ? 'From the history folder of the report: history.json (up to 32 MiB), optionally history-trend.json, duration-trend.json, categories-trend.json and retry-trend.json (up to 4 MiB each).'
                : 'The file Allure 3 appends to (historyPath in allurerc), up to 256 MiB, 1000 reports.'
            }
            placeholder={source === 'allure2' ? 'Choose files' : 'Choose a file'}
            multiple={source === 'allure2'}
            accept=".json,.jsonl,application/json"
            value={source === 'allure2' ? files : (files[0] ?? null)}
            onChange={(v) => {
              setFiles(Array.isArray(v) ? v : v ? [v] : []);
              setProblem(null);
            }}
            clearable
            disabled={!canWrite}
            error={problem}
            data-testid="history-files"
          />
          <SimpleGrid cols={{ base: 1, sm: 2 }}>
            <TextInput label="Branch" description="The branch your live runs report; their flaky detection reads history of the same branch." value={branch} onChange={(e) => setBranch(e.currentTarget.value)} maxLength={200} disabled={!canWrite} data-testid="history-branch" />
            <TextInput label="Environment" description="As the environment of live runs, if they set one." value={environment} onChange={(e) => setEnvironment(e.currentTarget.value)} maxLength={200} disabled={!canWrite} data-testid="history-environment" />
          </SimpleGrid>
          {!canWrite && (
            <Alert color="gray" variant="light">
              Importing history needs the permission to change project settings.
            </Alert>
          )}
          <Group>
            <Button leftSection={<IconHistory size={16} />} onClick={submit} loading={create.isPending} disabled={!canWrite || files.length === 0 || active} data-testid="history-submit">
              Import history
            </Button>
            {active && (
              <Text size="sm" c="dimmed">
                An import of this project is running.
              </Text>
            )}
          </Group>
        </Stack>
      </Card>
      <QueryState query={imports}>
        {() =>
          imports.data && imports.data.length > 0 ? (
            <Card withBorder radius="md" padding="md">
              <Title order={5} mb="xs">
                Recent imports
              </Title>
              {imports.data.map((i) => (
                <ImportRow key={i.id} i={i} />
              ))}
            </Card>
          ) : null
        }
      </QueryState>
    </Stack>
  );
}
