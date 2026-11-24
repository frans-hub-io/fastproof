import { Text } from '@mantine/core';

export function WorkflowSettings(props: { projectKey: string }) {
  return (
    <Text size="sm" c="dimmed" data-project={props.projectKey}>
      Not available yet
    </Text>
  );
}
