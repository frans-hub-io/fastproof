import type { ReactNode } from 'react';
import { Text } from '@mantine/core';

/** Gate evaluations over time (owner: gate analytics). */
export const GateHistory: (props: { project: string }) => ReactNode = () => (
  <Text size="sm" c="dimmed" data-testid="gate-history">
    Not available yet.
  </Text>
);
