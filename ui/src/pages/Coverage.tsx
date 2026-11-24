import { Text } from '@mantine/core';
import { PageHeader } from '../components/PageHeader';

export default function Coverage() {
  return (
    <>
      <PageHeader crumbs={[{ label: 'Projects', to: '/' }, { label: 'Coverage' }]} title="Coverage" />
      <Text size="sm" c="dimmed">
        Not available yet
      </Text>
    </>
  );
}
