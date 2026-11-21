import { Button } from '@mantine/core';
import { EmptyState } from '@franshub/ui-shell';
import { Link } from 'react-router';

export default function NotFoundPage() {
  return <EmptyState title="Page not found" description="The address does not match any FastProof page." action={<Button component={Link} to="/">Go to projects</Button>} />;
}
