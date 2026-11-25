import { Text } from '@mantine/core';
import { PageHeader } from '../components/PageHeader';

export default function SharedStep() {
  return (
    <>
      <PageHeader crumbs={[{ label: 'Projects', to: '/' }, { label: 'Shared step' }]} title="Shared step" />
      <Text size="sm" c="dimmed">
        Not available yet
      </Text>
    </>
  );
}
