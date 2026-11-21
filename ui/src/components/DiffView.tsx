import { Group, SegmentedControl, SimpleGrid, Stack, Text } from '@mantine/core';
import { useMemo, useState } from 'react';
import { diffLines } from '../lib/diff';

function pretty(s: string): string {
  const t = s.trim();
  if ((t.startsWith('{') && t.endsWith('}')) || (t.startsWith('[') && t.endsWith(']'))) {
    try {
      return JSON.stringify(JSON.parse(t), null, 2);
    } catch {
      return s;
    }
  }
  return s;
}

export function DiffView({ expected, actual }: { expected: string; actual: string }) {
  const [mode, setMode] = useState<'unified' | 'split'>('unified');
  const e = useMemo(() => pretty(expected), [expected]);
  const a = useMemo(() => pretty(actual), [actual]);
  const lines = useMemo(() => diffLines(e, a), [e, a]);
  return (
    <Stack gap="xs" data-testid="diff-view">
      <Group justify="space-between" wrap="wrap">
        <Group gap="md">
          <Text size="xs" c="var(--fp-diff-remove-fg)" fw={600}>
            − expected
          </Text>
          <Text size="xs" c="var(--fp-diff-add-fg)" fw={600}>
            + actual
          </Text>
        </Group>
        <SegmentedControl
          size="xs"
          value={mode}
          onChange={(v) => setMode(v === 'split' ? 'split' : 'unified')}
          data={[
            { value: 'unified', label: 'Unified' },
            { value: 'split', label: 'Side by side' },
          ]}
        />
      </Group>
      {mode === 'unified' ? (
        <pre className="fp-log" style={{ padding: '6px 0' }}>
          {lines.map((l, i) => (
            <span key={i} className="fp-diff-line" data-op={l.op}>
              {l.op === 'add' ? '+ ' : l.op === 'remove' ? '− ' : '  '}
              {l.text}
            </span>
          ))}
        </pre>
      ) : (
        <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="xs">
          <Stack gap={4}>
            <Text size="xs" c="dimmed">
              Expected
            </Text>
            <pre className="fp-log">{e}</pre>
          </Stack>
          <Stack gap={4}>
            <Text size="xs" c="dimmed">
              Actual
            </Text>
            <pre className="fp-log">{a}</pre>
          </Stack>
        </SimpleGrid>
      )}
    </Stack>
  );
}
