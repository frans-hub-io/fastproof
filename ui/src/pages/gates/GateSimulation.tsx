import { Alert, Anchor, Badge, Button, Group, NumberInput, Select, Stack, Table, TagsInput, Text, TextInput } from '@mantine/core';
import { useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { useSimulateGate } from '../../api/gatesHistory';
import { useAnalysisCapabilities } from '../../api/analysis';
import { useSettings } from '../../api/stageC';
import type { Gate } from '../../api/types-next';
import { GLOB, actualText, availableMetrics, ruleText, rulesValid } from '../../lib/gates';
import { RuleRows } from './RuleRows';

const BLANK: Gate = { name: 'Draft', enabled: true, rules: [{ metric: 'failed', op: 'lte', value: 0 }] };

/** A draft gate run over the newest finished launches; nothing is saved. */
export function GateSimulation({ project }: { project: string }) {
  const settings = useSettings(project);
  const caps = useAnalysisCapabilities(project).data;
  const [draft, setDraft] = useState<Gate>(BLANK);
  const [launches, setLaunches] = useState<number | string>(50);
  const [branch, setBranch] = useState('');
  const sim = useSimulateGate(project);
  const globsOk = (draft.branches ?? []).every((b) => GLOB.test(b));
  const count = typeof launches === 'number' ? launches : Number.NaN;
  const valid = rulesValid(draft.rules) && globsOk && count >= 1 && count <= 200;
  const metrics = availableMetrics(caps?.features.gateAnalytics === true, draft.rules);
  const out = sim.data;
  return (
    <Stack gap="md" data-testid="gate-simulation">
      <Text size="sm" c="dimmed">
        Try a gate on launches that already finished: nothing is saved and nobody is notified. Launches on other branches, still processing, or whose results were deleted are skipped.
      </Text>
      <Select
        label="Start from"
        placeholder="A blank draft"
        clearable
        data={(settings.data?.gates ?? []).map((g) => g.name)}
        value={null}
        onChange={(v) => {
          const g = settings.data?.gates.find((x) => x.name === v);
          if (g) setDraft(structuredClone(g));
        }}
        comboboxProps={{ withinPortal: true }}
        data-testid="gate-simulation-from"
      />
      <TagsInput
        label="Branches"
        description="Globs; empty applies the draft to every branch"
        placeholder="main, release/*"
        value={draft.branches ?? []}
        onChange={(v) => setDraft({ ...draft, branches: v.length ? v : null })}
        error={globsOk ? null : 'Use branch names or globs like release/*'}
        maxTags={20}
        comboboxProps={{ withinPortal: true }}
      />
      <RuleRows rules={draft.rules} onChange={(rules) => setDraft({ ...draft, rules })} metrics={metrics} />
      <Group gap="sm" align="flex-end" wrap="wrap">
        <NumberInput label="Launches" value={launches} onChange={setLaunches} min={1} max={200} allowDecimal={false} w={120} data-testid="gate-simulation-launches" />
        <TextInput label="Only branch" placeholder="any" value={branch} onChange={(e) => setBranch(e.currentTarget.value)} maxLength={256} />
        <Button
          onClick={() => sim.mutate({ gate: { ...draft, name: draft.name || 'Draft' }, launches: count, branch: branch.trim() || null })}
          disabled={!valid}
          loading={sim.isPending}
          data-testid="gate-simulation-run"
        >
          Run simulation
        </Button>
      </Group>
      {sim.isError && (
        <Alert color="red" title="Simulation failed">
          {errorMessage(sim.error)}
        </Alert>
      )}
      {out && (
        <Stack gap="sm" data-testid="gate-simulation-result">
          <Group gap="xs">
            <Badge size="lg" variant="light" color={out.failed ? 'red' : 'green'} radius="sm" style={{ textTransform: 'none' }}>
              {String(out.failed)} of {String(out.evaluated)} would fail
            </Badge>
            {out.skipped > 0 && (
              <Badge size="lg" variant="default" radius="sm" style={{ textTransform: 'none' }}>
                {String(out.skipped)} skipped
              </Badge>
            )}
          </Group>
          {out.items.length === 0 ? (
            <Text size="sm" c="dimmed">
              No launch could be evaluated.
            </Text>
          ) : (
            <div className="fp-table-wrap">
              <Table verticalSpacing="xs" highlightOnHover>
                <Table.Thead>
                  <Table.Tr>
                    <Table.Th>Launch</Table.Th>
                    <Table.Th>Branch</Table.Th>
                    <Table.Th>Verdict</Table.Th>
                    <Table.Th>Failed rules</Table.Th>
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {out.items.map((i) => (
                    <Table.Tr key={i.launchId} data-testid="gate-simulation-row">
                      <Table.Td>
                        <Anchor component={Link} to={`/p/${project}/launches/${i.launchId}`} size="sm">
                          #{i.launchNumber}
                        </Anchor>
                      </Table.Td>
                      <Table.Td className="fp-mono">{i.branch ?? '—'}</Table.Td>
                      <Table.Td>
                        <Badge variant="light" color={i.status === 'passed' ? 'green' : 'red'} radius="sm" style={{ textTransform: 'none' }}>
                          {i.status}
                        </Badge>
                      </Table.Td>
                      <Table.Td>
                        <Stack gap={2}>
                          {i.results
                            .filter((r) => !r.ok)
                            .map((r, k) => (
                              <Text key={k} size="xs">
                                {ruleText(r.rule)} <span className="fp-mono">(actual {actualText(r.rule.metric, r.actual)})</span>
                              </Text>
                            ))}
                        </Stack>
                      </Table.Td>
                    </Table.Tr>
                  ))}
                </Table.Tbody>
              </Table>
            </div>
          )}
        </Stack>
      )}
    </Stack>
  );
}
