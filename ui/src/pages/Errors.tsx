import { Text } from '@mantine/core';
import { useParams } from 'react-router';
import { PageHeader } from '../components/PageHeader';
import { useProject } from '../api/hooks';

/** Unique errors (FastProof Analysis). The area's branch fills this page. */
export default function ErrorsPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  return (
    <>
      <PageHeader crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Unique errors' }]} title="Unique errors" subtitle="Errors of this project by fingerprint, with their state across branches" />
      <Text size="sm" c="dimmed" data-testid="analysis-page-stub">
        Not available yet.
      </Text>
    </>
  );
}
