import type { ReactNode } from 'react';
import { Text } from '@mantine/core';

/** The project's FastProof Analysis settings (owner: suggestions; flaky hints add theirs). */
export const AnalysisSettings: (props: { project: string; canWrite: boolean }) => ReactNode = () => (
  <Text size="sm" c="dimmed" data-testid="analysis-settings">
    Not available yet.
  </Text>
);
