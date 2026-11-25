import { Text } from '@mantine/core';
import { PageHeader } from '../components/PageHeader';

export default function Requirements() {
  return (
    <>
      <PageHeader crumbs={[{ label: 'Projects', to: '/' }, { label: 'Requirements' }]} title="Requirements" />
      <Text size="sm" c="dimmed">
        Not available yet
      </Text>
    </>
  );
}
