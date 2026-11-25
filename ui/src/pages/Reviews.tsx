import { Text } from '@mantine/core';
import { PageHeader } from '../components/PageHeader';

export default function Reviews() {
  return (
    <>
      <PageHeader crumbs={[{ label: 'Projects', to: '/' }, { label: 'Reviews' }]} title="Reviews" />
      <Text size="sm" c="dimmed">
        Not available yet
      </Text>
    </>
  );
}
