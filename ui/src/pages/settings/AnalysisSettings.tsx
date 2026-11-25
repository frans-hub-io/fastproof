import { Alert, Badge, Button, Card, Fieldset, Group, NumberInput, SimpleGrid, Stack, Switch, Table, Text, Title } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconAlertTriangle, IconPlayerPlay } from '@tabler/icons-react';
import { useState, type ReactNode } from 'react';
import { errorMessage } from '../../api/client';
import { useSaveSettings, useSettings } from '../../api/stageC';
import { useLatestBacktest, useRunBacktest, type AnalysisSettingsDoc, type BacktestReport } from '../../api/suggestions';
import type { ProjectSettings } from '../../api/types-next';
import { QueryState } from '../../components/QueryState';
import { formatDate } from '../../lib/format';

/** The `analysis` area with every part filled in. */
interface AnalysisDoc {
  enabled: boolean;
  memoryDays: number;
  resolveAfterLaunches: number;
  suggest: { minScore: number; maxCandidates: number };
  autoApply: { enabled: boolean; bands: NonNullable<NonNullable<AnalysisSettingsDoc['autoApply']>['bands']> };
  backtest: { nightly: boolean };
  flaky: { hintMinScore: number };
}

/** The defaults of `analysis` (as the server has them). */
const ANALYSIS_DEFAULTS: AnalysisDoc = {
  enabled: true,
  memoryDays: 365,
  resolveAfterLaunches: 10,
  suggest: { minScore: 0.4, maxCandidates: 3 },
  autoApply: { enabled: false, bands: ['same'] },
  backtest: { nightly: true },
  flaky: { hintMinScore: 0.2 },
};

type WithAnalysis = ProjectSettings & { analysis?: Partial<AnalysisSettingsDoc> };

/** The project's `analysis` area with every part filled in. */
function analysisOf(s: ProjectSettings): AnalysisDoc {
  const a = (s as WithAnalysis).analysis ?? {};
  return {
    ...ANALYSIS_DEFAULTS,
    ...a,
    suggest: { ...ANALYSIS_DEFAULTS.suggest, ...a.suggest },
    autoApply: { ...ANALYSIS_DEFAULTS.autoApply, ...a.autoApply },
    backtest: { ...ANALYSIS_DEFAULTS.backtest, ...a.backtest },
    flaky: { ...ANALYSIS_DEFAULTS.flaky, ...a.flaky },
  };
}

function Section({ title, description, children }: { title: string; description?: ReactNode; children: ReactNode }) {
  return (
    <Card withBorder radius="md" padding="md">
      <Stack gap="sm">
        <div>
          <Title order={5}>{title}</Title>
          {description && (
            <Text size="sm" c="dimmed">
              {description}
            </Text>
          )}
        </div>
        {children}
      </Stack>
    </Card>
  );
}

function num(v: number | string, fallback: number): number {
  const n = typeof v === 'number' ? v : Number(v);
  return Number.isFinite(n) ? n : fallback;
}

function range(v: number | string, min: number, max: number): string | null {
  const n = typeof v === 'number' ? v : Number(v);
  return v === '' || !Number.isFinite(n) || n < min || n > max ? `${String(min)} to ${String(max)}` : null;
}

/** The latest backtest by band. */
export function BacktestView({ report }: { report: BacktestReport }) {
  return (
    <Stack gap="xs" data-testid="backtest-report">
      <Text size="sm">
        {report.decisions} decisions{report.from && report.to ? ` from ${formatDate(report.from)} to ${formatDate(report.to)}` : ''}; {Math.round(report.coverage * 100)}% had a suggestion
        {report.truncated ? ' (truncated)' : ''}. Made {formatDate(report.createdAt)}.
      </Text>
      <Table.ScrollContainer minWidth={420}>
        <Table fz="sm" verticalSpacing={4} data-testid="backtest-bands">
          <Table.Thead>
            <Table.Tr>
              <Table.Th>Band</Table.Th>
              <Table.Th ta="right">Suggested</Table.Th>
              <Table.Th ta="right">Right type</Table.Th>
              <Table.Th ta="right">Right group</Table.Th>
              <Table.Th ta="right">Precision</Table.Th>
            </Table.Tr>
          </Table.Thead>
          <Table.Tbody>
            {report.bands.map((b) => (
              <Table.Tr key={b.band} data-band={b.band}>
                <Table.Td>{b.band}</Table.Td>
                <Table.Td ta="right">{b.suggested}</Table.Td>
                <Table.Td ta="right">{b.correct}</Table.Td>
                <Table.Td ta="right">{b.correctGroup}</Table.Td>
                <Table.Td ta="right">
                  {b.precision === null || b.precision === undefined ? '–' : `${(b.precision * 100).toFixed(1)}%`}
                  {b.precision !== null && b.precision !== undefined && !b.shown ? (
                    <Text span size="xs" c="dimmed">
                      {' '}
                      (fewer than 50)
                    </Text>
                  ) : null}
                </Table.Td>
              </Table.Tr>
            ))}
          </Table.Tbody>
        </Table>
      </Table.ScrollContainer>
      {report.confusions.length > 0 && (
        <Text size="xs" c="dimmed">
          Most frequent misses: {report.confusions.slice(0, 3).map((c) => `${c.suggested} suggested for ${c.expected} (${String(c.count)})`).join('; ')}
        </Text>
      )}
    </Stack>
  );
}

function Backtest({ project, canWrite, enabled }: { project: string; canWrite: boolean; enabled: boolean }) {
  const latest = useLatestBacktest(project);
  const run = useRunBacktest(project);
  return (
    <Section
      title="Backtest"
      description="Replays the decisions people made, in time order, against the memory built from the decisions before each one, with the same scoring as live suggestions. It shows how often each band named the type people chose. The percentage is shown on suggestions once a band has 50 decisions."
    >
      <QueryState query={latest}>
        {() =>
          latest.data ? (
            <BacktestView report={latest.data} />
          ) : (
            <Text size="sm" c="dimmed" data-testid="backtest-none">
              No backtest yet.
            </Text>
          )
        }
      </QueryState>
      {canWrite && (
        <Group>
          <Button
            size="xs"
            variant="light"
            leftSection={<IconPlayerPlay size={14} />}
            disabled={!enabled}
            loading={run.isPending}
            onClick={() =>
              run.mutate(undefined, {
                onSuccess: () => notifications.show({ color: 'green', title: 'Backtest queued', message: 'The report shows here when it is done (the last 180 days).' }),
                onError: (e) => notifications.show({ color: 'red', title: 'Backtest not started', message: errorMessage(e) }),
              })
            }
            data-testid="backtest-run"
          >
            Run backtest
          </Button>
        </Group>
      )}
    </Section>
  );
}

function Form({ project, settings, canWrite }: { project: string; settings: ProjectSettings; canWrite: boolean }) {
  const a = analysisOf(settings);
  const [enabled, setEnabled] = useState(a.enabled);
  const [minScore, setMinScore] = useState<number | string>(a.suggest.minScore);
  const [maxCandidates, setMaxCandidates] = useState<number | string>(a.suggest.maxCandidates);
  const [auto, setAuto] = useState(a.autoApply.enabled);
  const [memoryDays, setMemoryDays] = useState<number | string>(a.memoryDays);
  const [resolveAfter, setResolveAfter] = useState<number | string>(a.resolveAfterLaunches);
  const [nightly, setNightly] = useState(a.backtest.nightly);
  const save = useSaveSettings(project);
  const errors = {
    minScore: range(minScore, 0.4, 0.95),
    maxCandidates: range(maxCandidates, 1, 3),
    memoryDays: range(memoryDays, 30, 3650),
    resolveAfter: range(resolveAfter, 1, 100),
  };
  const valid = Object.values(errors).every((e) => e === null);
  const submit = () => {
    if (!valid) return;
    const next: AnalysisDoc = {
      ...a,
      enabled,
      memoryDays: num(memoryDays, a.memoryDays),
      resolveAfterLaunches: num(resolveAfter, a.resolveAfterLaunches),
      suggest: { minScore: num(minScore, a.suggest.minScore), maxCandidates: num(maxCandidates, a.suggest.maxCandidates) },
      autoApply: { enabled: auto, bands: ['same'] },
      backtest: { nightly },
    };
    save.mutate({ ...settings, analysis: next } as ProjectSettings, {
      onSuccess: () => notifications.show({ color: 'green', message: 'Analysis settings saved' }),
      onError: (e) => notifications.show({ color: 'red', title: 'Analysis settings not saved', message: errorMessage(e) }),
    });
  };
  return (
    <Stack gap="md" data-testid="analysis-settings">
      <Fieldset variant="unstyled" disabled={!canWrite}>
        <Stack gap="md">
          <Section title="FastProof Analysis" description="Suggestions, unique errors and the other analysis of finished launches. Suggestions come from what people decided before: a transparent nearest-decision search with a fixed weight table, not a self-learning model.">
            <Switch label="Analyse this project" checked={enabled} onChange={(e) => setEnabled(e.currentTarget.checked)} data-testid="analysis-enabled" />
            <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="sm">
              <NumberInput label="Remember decisions for (days)" description="What people decided is kept this long, even after raw results are deleted" min={30} max={3650} value={memoryDays} onChange={setMemoryDays} error={errors.memoryDays} data-testid="analysis-memory-days" />
              <NumberInput label="Resolve an error after (launches)" description="Launches without the error on each branch before it counts as resolved" min={1} max={100} value={resolveAfter} onChange={setResolveAfter} error={errors.resolveAfter} data-testid="analysis-resolve-after" />
            </SimpleGrid>
          </Section>
          <Section title="Suggestions" description="Bands: same (the same error decided the same way before), high (score 0.70 and more), low (from the minimum score). Up to three per cluster, one per defect type.">
            <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="sm">
              <NumberInput label="Minimum score" description="Lower scores are not shown (0.40 to 0.95)" min={0.4} max={0.95} step={0.05} decimalScale={2} value={minScore} onChange={setMinScore} error={errors.minScore} data-testid="analysis-min-score" />
              <NumberInput label="Suggestions per cluster" min={1} max={3} value={maxCandidates} onChange={setMaxCandidates} error={errors.maxCandidates} data-testid="analysis-max-candidates" />
            </SimpleGrid>
          </Section>
          <Section title="Auto-apply" description="After each launch, clusters nobody decided whose first suggestion is in the same band get its defect type by themselves: no comment, issue or mute. People and agents can overwrite them, and each launch's auto decisions are one batch you can undo.">
            <Switch label="Apply same-band suggestions automatically" checked={auto} onChange={(e) => setAuto(e.currentTarget.checked)} data-testid="analysis-auto-apply" />
            {auto && (
              <Alert color="yellow" variant="light" icon={<IconAlertTriangle size={16} />} data-testid="analysis-auto-warning">
                Defects will be set without a person looking. Check the backtest first: the same band should be right almost always in this project. Auto decisions never teach the memory; confirm them to make them yours.
              </Alert>
            )}
            <Group gap="xs">
              <Text size="sm">Bands:</Text>
              <Badge variant="light" style={{ textTransform: 'none' }}>
                same
              </Badge>
            </Group>
          </Section>
          <Section title="Nightly backtest">
            <Switch label="Run a backtest every night when there are new decisions" checked={nightly} onChange={(e) => setNightly(e.currentTarget.checked)} data-testid="analysis-nightly" />
          </Section>
          {canWrite && (
            <Group>
              <Button onClick={submit} loading={save.isPending} disabled={!valid} data-testid="analysis-save">
                Save
              </Button>
            </Group>
          )}
        </Stack>
      </Fieldset>
      <Backtest project={project} canWrite={canWrite} enabled={a.enabled} />
    </Stack>
  );
}

/** The project's FastProof Analysis settings, with the latest backtest. */
export function AnalysisSettings({ project, canWrite }: { project: string; canWrite: boolean }) {
  const settings = useSettings(project);
  return <QueryState query={settings}>{() => settings.data && <Form key={JSON.stringify(analysisOf(settings.data))} project={project} settings={settings.data} canWrite={canWrite} />}</QueryState>;
}
