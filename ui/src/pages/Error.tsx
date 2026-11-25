import { Text } from '@mantine/core';
import { useParams } from 'react-router';
import { PageHeader } from '../components/PageHeader';
import { useProject } from '../api/hooks';

/** Unique error (FastProof Analysis). The area's branch fills this page. */
export default function ErrorPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  return (
    <>
      <PageHeader crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Unique error' }]} title="Unique error" subtitle="One error: where and when it occurs, its decisions and history" />
      <Text size="sm" c="dimmed" data-testid="analysis-page-stub">
        Not available yet.
      </Text>
    </>
  );
}
