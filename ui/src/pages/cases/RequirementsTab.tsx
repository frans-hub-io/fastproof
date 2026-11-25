import { Text } from '@mantine/core';

export function RequirementsTab(props: { projectKey: string; caseNumber: number }) {
  return (
    <Text size="sm" c="dimmed" data-project={props.projectKey}>
      Not available yet
    </Text>
  );
}
