import { Text } from '@mantine/core';
import { useParams } from 'react-router';
import { PageHeader } from '../components/PageHeader';
import { useProject } from '../api/hooks';

/** Analytics (FastProof Analysis). The area's branch fills this page. */
export default function AnalyticsPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  return (
    <>
      <PageHeader crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Analytics' }]} title="Analytics" subtitle="Failure trends, slow tests and retry costs" />
      <Text size="sm" c="dimmed" data-testid="analysis-page-stub">
        Not available yet.
      </Text>
    </>
  );
}
