import { Badge, Group, MultiSelect, NumberInput, SegmentedControl, Stack, Text, Textarea, TextInput, Tooltip } from '@mantine/core';
import { LOG_LEVELS, LOG_STREAMS, type LogDraft, logTextError, patternError } from './logDraft';

export function LogConditions({ d, set, regexError }: { d: LogDraft; set: <K extends keyof LogDraft>(k: K, v: LogDraft[K]) => void; regexError: (s: string) => string | null }) {
  const err = logTextError(d, regexError);
  return (
    <Stack gap="xs" data-testid="rule-log">
      <Group gap="xs" justify="space-between" wrap="wrap">
        <Text size="sm" fw={500}>
          Log lines of failed attempts
        </Text>
        <SegmentedControl
          size="xs"
          value={d.logMode}
          onChange={(v) => set('logMode', v === 'regex' || v === 'contains' ? v : 'off')}
          data={[
            { value: 'off', label: 'Off' },
            { value: 'contains', label: 'Contains' },
            { value: 'regex', label: 'Regex' },
          ]}
          data-testid="rule-log-mode"
        />
      </Group>
      {d.logMode !== 'off' && (
        <>
          <Textarea
            aria-label={d.logMode === 'regex' ? 'Log line regex' : 'Text in a log line'}
            description={d.logMode === 'regex' ? 'Rust regex syntax, matched against each line (linear time, size limited)' : 'Case-insensitive text in a line'}
            autosize
            minRows={1}
            maxRows={4}
            value={d.logText}
            onChange={(e) => set('logText', e.currentTarget.value)}
            error={err}
            classNames={{ input: 'fp-mono' }}
            data-testid="rule-log-text"
          />
          <Group grow gap="xs" wrap="wrap" align="flex-start">
            <MultiSelect label="Levels" placeholder="Any level" data={LOG_LEVELS.map((l) => ({ value: l, label: l }))} value={d.logLevels} onChange={(v) => set('logLevels', v)} comboboxProps={{ withinPortal: true }} miw={160} data-testid="rule-log-levels" />
            <MultiSelect label="Streams" placeholder="Any stream" data={LOG_STREAMS.map((s) => ({ value: s, label: s }))} value={d.logStreams} onChange={(v) => set('logStreams', v)} comboboxProps={{ withinPortal: true }} miw={160} data-testid="rule-log-streams" />
          </Group>
          <NumberInput
            label="Read the last KiB of each attempt"
            description="1 to 1024; empty for the server default (256 KiB)"
            placeholder="256"
            min={1}
            max={1024}
            allowDecimal={false}
            value={d.logLastKiB}
            onChange={(v) => set('logLastKiB', typeof v === 'number' ? v : '')}
            w={320}
            data-testid="rule-log-last"
          />
          <Text size="xs" c="dimmed">
            Captured stdout and stderr of uploaded results have no levels: a rule with levels skips them.
          </Text>
        </>
      )}
      {!!d.signals?.length && (
        <Group gap={6} wrap="wrap">
          <Text size="xs" c="dimmed">
            Signals:
          </Text>
          {d.signals.map((s) => (
            <Badge key={`${s.source}:${s.kind ?? ''}`} size="xs" variant="outline" color="gray" style={{ textTransform: 'none' }}>
              {s.source}
              {s.kind ? ` · ${s.kind}` : ''}
            </Badge>
          ))}
        </Group>
      )}
    </Stack>
  );
}

export function PatternAction({ d, set }: { d: LogDraft; set: <K extends keyof LogDraft>(k: K, v: LogDraft[K]) => void }) {
  return (
    <TextInput
      label="Mark with a log pattern"
      description="Only a mark (shown on the result, filterable, counted on dashboards); combinable with the other actions"
      placeholder="for example oom or db-timeout"
      maxLength={64}
      value={d.pattern}
      onChange={(e) => set('pattern', e.currentTarget.value)}
      error={patternError(d.pattern)}
      classNames={{ input: 'fp-mono' }}
      data-testid="rule-pattern"
    />
  );
}

/** When a rule runs, as a badge. */
export function PhaseBadge({ phase }: { phase?: string }) {
  const after = phase === 'afterLaunch';
  return (
    <Tooltip label={after ? 'Runs once the launch has finished: reads logs and signals, marks patterns' : 'Runs as results are ingested'} withinPortal>
      <Badge size="xs" variant="light" color={after ? 'fhPurple' : 'gray'} style={{ textTransform: 'none' }} data-testid="rule-phase">
        {after ? 'after launch' : 'on ingest'}
      </Badge>
    </Tooltip>
  );
}
