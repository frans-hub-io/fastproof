import type { ReactNode } from 'react';
import { Text } from '@mantine/core';
import type { ResultDetail } from '../../api/types';

/** The triage journal of a result with undo (owner: suggestions). */
export const TriageHistory: (props: { project: string; r: ResultDetail }) => ReactNode = () => (
  <Text size="sm" c="dimmed" data-testid="triage-history">
    Not available yet.
  </Text>
);
