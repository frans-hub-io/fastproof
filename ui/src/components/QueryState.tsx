import { Center, Loader, Stack, Text } from '@mantine/core';
import { EmptyState, ErrorState } from '@franshub/ui-shell';
import type { ReactNode } from 'react';
import { ApiError, errorMessage } from '../api/client';

export function Loading({ label = 'Loading…' }: { label?: string }) {
  return (
    <Center py={48}>
      <Stack align="center" gap="xs">
        <Loader size="sm" />
        <Text size="sm" c="dimmed">
          {label}
        </Text>
      </Stack>
    </Center>
  );
}

interface QueryLike {
  isPending: boolean;
  isError: boolean;
  error: unknown;
  refetch: () => unknown;
}

export function QueryState({ query, children, notFound }: { query: QueryLike; children: () => ReactNode; notFound?: string }) {
  if (query.isPending) return <Loading />;
  if (query.isError) {
    if (query.error instanceof ApiError && query.error.status === 404) {
      if (query.error.message === 'resource not found') return <EmptyState title="Not available on this server" description="This FastProof server does not provide this API yet. Upgrade the server to use this page." />;
      return <EmptyState title={notFound ?? 'Not found'} description={query.error.message} />;
    }
    if (query.error instanceof ApiError && query.error.status === 403) return <ErrorState title="Access denied" message={errorMessage(query.error)} />;
    return <ErrorState message={errorMessage(query.error)} onRetry={() => void query.refetch()} />;
  }
  return <>{children()}</>;
}
