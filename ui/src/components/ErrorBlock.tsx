import { Anchor, Collapse, Stack, Text } from '@mantine/core';
import { useState } from 'react';
import type { StatusError } from '../api/types';
import { hasAnsi } from '../lib/ansi';
import { AnsiText } from './Ansi';
import { CopyValue } from './CopyButton';
import { DiffView } from './DiffView';

export function ErrorBlock({ error, compact = false }: { error: StatusError; compact?: boolean }) {
  const [trace, setTrace] = useState(!compact && !error.message);
  const hasDiff = typeof error.expected === 'string' && typeof error.actual === 'string' && error.expected !== error.actual;
  return (
    <Stack gap="xs" data-testid="error-block">
      {error.message && (
        <div style={{ position: 'relative' }}>
          {hasAnsi(error.message) ? (
            <AnsiText text={error.message} maxHeight={compact ? 200 : 360} />
          ) : (
            <pre className="fp-log" style={{ borderColor: 'var(--mantine-color-error)', maxHeight: compact ? 200 : 360 }} data-testid="error-message">
              {error.message}
            </pre>
          )}
          <div style={{ position: 'absolute', top: 6, right: 6 }}>
            <CopyValue value={error.message} label="Copy the message" />
          </div>
        </div>
      )}
      {hasDiff && <DiffView expected={error.expected ?? ''} actual={error.actual ?? ''} />}
      {error.trace && (
        <>
          <Anchor component="button" type="button" size="xs" onClick={() => setTrace((t) => !t)} style={{ alignSelf: 'flex-start' }}>
            {trace ? 'Hide the stack trace' : 'Show the stack trace'}
          </Anchor>
          <Collapse expanded={trace}>
            <AnsiText text={error.trace} maxHeight={420} testId="error-trace" />
          </Collapse>
        </>
      )}
      {!error.message && !error.trace && (
        <Text size="sm" c="dimmed">
          No error details were reported.
        </Text>
      )}
    </Stack>
  );
}
