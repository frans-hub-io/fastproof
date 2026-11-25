import { ActionIcon, Button, Card, Group, NumberInput, Select, SimpleGrid, Stack, Text, TextInput } from '@mantine/core';
import { IconPlus, IconTrash } from '@tabler/icons-react';
import type { GateMetric, GateRule } from '../../api/types-next';
import { MAX_RULES, METRICS, OPS, metricOf, ruleError, ruleHint } from '../../lib/gates';

/** The rule rows of a gate, shared by the gate editor and the simulation. */
export function RuleRows({ rules, onChange, metrics }: { rules: GateRule[]; onChange: (rules: GateRule[]) => void; metrics: GateMetric[] }) {
  const setRule = (i: number, r: Partial<GateRule>) => onChange(rules.map((x, j) => (j === i ? { ...x, ...r } : x)));
  return (
    <Stack gap="sm">
      {rules.map((r, i) => {
        const m = metricOf(r.metric);
        const err = ruleError(r);
        return (
          <Card key={i} withBorder radius="sm" padding="xs" data-testid="gate-rule-row">
            <Stack gap={6}>
              <SimpleGrid cols={{ base: 2, sm: 4 }} spacing="xs">
                <Select aria-label="Metric" data={METRICS.filter((x) => metrics.includes(x.value)).map(({ value, label }) => ({ value, label }))} value={r.metric} onChange={(v) => v && setRule(i, { metric: v })} allowDeselect={false} comboboxProps={{ withinPortal: true }} data-testid="gate-rule-metric" />
                <Select aria-label="Comparison" data={OPS} value={r.op} onChange={(v) => v && setRule(i, { op: v })} allowDeselect={false} comboboxProps={{ withinPortal: true }} />
                <NumberInput
                  aria-label="Value"
                  value={r.value}
                  onChange={(v) => setRule(i, { value: typeof v === 'number' ? v : Number.NaN })}
                  min={r.baseline ? undefined : 0}
                  max={r.metric === 'passRate' ? 100 : undefined}
                  decimalScale={r.metric === 'passRate' ? 1 : 0}
                  rightSection={
                    <Text size="xs" c="dimmed">
                      {m.unit === 'seconds' ? 's' : m.unit === '%' ? '%' : ''}
                    </Text>
                  }
                  data-testid="gate-rule-value"
                />
                <TextInput aria-label="Baseline branch" placeholder="baseline (optional)" value={r.baseline ?? ''} onChange={(e) => setRule(i, { baseline: e.currentTarget.value || null })} />
              </SimpleGrid>
              <Group justify="space-between" wrap="nowrap" gap="xs">
                <Text size="xs" c={err ? 'red' : 'dimmed'}>
                  {err ?? ruleHint(r)}
                </Text>
                <ActionIcon variant="subtle" color="red" aria-label="Remove the rule" onClick={() => onChange(rules.filter((_, j) => j !== i))} disabled={rules.length === 1}>
                  <IconTrash size={16} />
                </ActionIcon>
              </Group>
            </Stack>
          </Card>
        );
      })}
      {rules.length < MAX_RULES && (
        <Group>
          <Button size="xs" variant="light" leftSection={<IconPlus size={14} />} onClick={() => onChange([...rules, { metric: 'failed', op: 'lte', value: 0 }])} data-testid="gate-add-rule">
            Add rule
          </Button>
        </Group>
      )}
    </Stack>
  );
}
