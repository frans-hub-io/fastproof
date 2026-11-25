import { Text } from '@mantine/core';
import { PageHeader } from '../components/PageHeader';

export default function Requirement() {
  return (
    <>
      <PageHeader crumbs={[{ label: 'Projects', to: '/' }, { label: 'Requirement' }]} title="Requirement" />
      <Text size="sm" c="dimmed">
        Not available yet
      </Text>
    </>
  );
}
