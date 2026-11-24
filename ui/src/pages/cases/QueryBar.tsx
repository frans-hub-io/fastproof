import { Button, Group, Loader, Stack, Text } from '@mantine/core';
import { useDebouncedValue } from '@mantine/hooks';
import { lazy, Suspense, useEffect } from 'react';
import { useQuerySchema, useValidateQuery, type QueryError } from '../../api/queries';
import { countLabel, errorText } from '../../lib/tm/fpql';

const QueryEditor = lazy(() => import('./QueryEditor'));

const QUERY_PLACEHOLDER = 'status = active and automated = false and tag = smoke';

export interface QueryBarProps {
  project: string;
  value: string;
  onChange: (value: string) => void;
  /** Called on Enter and with the apply button. Not called while the text is known to be invalid. */
  onApply: (value: string) => void;
  /** An `invalid_query` error of the applied query; shown while the text still equals it. */
  serverError?: QueryError | null;
  /** `null` hides the apply button (the dialog around the bar has its own). */
  applyLabel?: string | null;
  onValidity?: (state: 'empty' | 'checking' | 'ok' | 'invalid') => void;
}

/** One-line FPQL editor with completion from the schema, debounced validation and the match count. */
export function QueryBar({ project, value, onChange, onApply, serverError, applyLabel = 'Apply', onValidity }: QueryBarProps) {
  const schema = useQuerySchema(project);
  const [debounced] = useDebouncedValue(value, 400);
  const validation = useValidateQuery(project, debounced);
  const text = value.trim();
  const current = text && debounced === value && validation.data && !validation.isPlaceholderData ? validation.data : null;
  const error = current ? (current.ok ? null : (current.error ?? null)) : (serverError ?? null);
  const invalid = !!error;
  const state = !text ? 'empty' : current ? (current.ok ? 'ok' : 'invalid') : serverError ? 'invalid' : 'checking';
  useEffect(() => onValidity?.(state), [state, onValidity]);
  const apply = (v: string) => {
    if (!invalid || v.trim() !== text) onApply(v);
  };
  return (
    <Stack gap={4} style={{ minWidth: 0 }} data-testid="query-box">
      <Group gap="xs" wrap="wrap" align="flex-start">
        <div style={{ flex: '1 1 220px', minWidth: 0 }}>
          <Suspense fallback={<div style={{ height: 34, border: '1px solid var(--mantine-color-default-border)', borderRadius: 'var(--mantine-radius-sm)' }} aria-hidden />}>
            <QueryEditor value={value} onChange={onChange} onApply={apply} schema={schema.data} error={error} placeholder={QUERY_PLACEHOLDER} />
          </Suspense>
        </div>
        {applyLabel !== null && (
          <Button size="sm" onClick={() => apply(value)} disabled={invalid} data-testid="query-apply">
            {applyLabel}
          </Button>
        )}
      </Group>
      <Group gap="xs" wrap="nowrap" align="flex-start" mih={18}>
        {error ? (
          <Text size="xs" c="red" role="alert" data-testid="query-error" style={{ overflowWrap: 'anywhere' }}>
            {errorText(error)}
          </Text>
        ) : current?.ok && current.count != null ? (
          <Text size="xs" c="dimmed" data-testid="query-count" aria-live="polite">
            {countLabel(current.count, !!current.countCapped)} {current.count === 1 && !current.countCapped ? 'case matches' : 'cases match'}
          </Text>
        ) : text && validation.isFetching ? (
          <Loader size={12} aria-label="Checking the query" />
        ) : null}
      </Group>
    </Stack>
  );
}
