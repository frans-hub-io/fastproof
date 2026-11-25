import type { ReactNode } from 'react';
import { Text } from '@mantine/core';

/** A draft gate run over stored launches (owner: gate analytics). */
export const GateSimulation: (props: { project: string }) => ReactNode = () => (
  <Text size="sm" c="dimmed" data-testid="gate-simulation">
    Not available yet.
  </Text>
);
