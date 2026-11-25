import { Text } from '@mantine/core';
import { PageHeader } from '../components/PageHeader';

export default function SharedSteps() {
  return (
    <>
      <PageHeader crumbs={[{ label: 'Projects', to: '/' }, { label: 'Shared steps' }]} title="Shared steps" />
      <Text size="sm" c="dimmed">
        Not available yet
      </Text>
    </>
  );
}
