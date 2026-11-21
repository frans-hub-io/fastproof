import { Card, Group, SimpleGrid, Stack, Text, TextInput } from '@mantine/core';
import { EmptyState } from '@franshub/ui-shell';
import { IconSearch } from '@tabler/icons-react';
import { useState } from 'react';
import { Link } from 'react-router';
import { useLatestLaunch, useProjects } from '../api/hooks';
import type { LastLaunch, Project } from '../api/types';
import { PageHeader } from '../components/PageHeader';
import { passRate } from '../lib/counts';
import { QueryState } from '../components/QueryState';
import { CountsInline, LaunchStatusBadge, StatusBar } from '../components/Status';
import { formatRelative } from '../lib/format';

function LastLaunchBlock({ project }: { project: Project }) {
  const needFetch = project.lastLaunch === undefined && project.launchCount > 0;
  const latest = useLatestLaunch(project.key, needFetch);
  const last: LastLaunch | null | undefined = project.lastLaunch ?? (latest.data ? { ...latest.data } : latest.data);
  if (!project.launchCount) {
    return (
      <Text size="sm" c="dimmed">
        No launches yet. Upload results from CI to start.
      </Text>
    );
  }
  if (!last) return <StatusBar counts={{ total: 0, passed: 0, failed: 0, broken: 0, skipped: 0, unknown: 0, flaky: 0, muted: 0 }} />;
  return (
    <Stack gap={8}>
      <Group justify="space-between" gap="xs" wrap="nowrap">
        <Text size="sm" fw={600} className="fh-truncate">
          #{last.number} {last.name}
        </Text>
        <LaunchStatusBadge status={last.status} />
      </Group>
      <StatusBar counts={last.counts} live={last.status === 'running'} />
      <Group justify="space-between" gap="xs">
        <CountsInline counts={last.counts} />
        <Text size="xs" c="dimmed">
          {Math.round(passRate(last.counts) * 100)}% passed
        </Text>
      </Group>
    </Stack>
  );
}

function ProjectCard({ project }: { project: Project }) {
  return (
    <Card withBorder padding="md" radius="lg" component={Link} to={`/p/${project.key}`} data-testid="project-card" aria-label={`Project ${project.name}`} style={{ textDecoration: 'none', color: 'inherit' }}>
      <Stack gap="sm" h="100%">
        <Stack gap={2}>
          <Group justify="space-between" wrap="nowrap" gap="xs">
            <Text fw={800} size="lg" className="fh-truncate">
              {project.name}
            </Text>
            <Text size="xs" c="dimmed" className="fp-mono">
              {project.key}
            </Text>
          </Group>
          <Text size="sm" c="dimmed" lineClamp={2} mih={40}>
            {project.description || 'No description'}
          </Text>
        </Stack>
        <div style={{ flex: 1 }} />
        <LastLaunchBlock project={project} />
        <Text size="xs" c="dimmed">
          {project.launchCount} launches · last {formatRelative(project.lastLaunchAt)}
        </Text>
      </Stack>
    </Card>
  );
}

export default function ProjectsPage() {
  const q = useProjects();
  const [filter, setFilter] = useState('');
  return (
    <>
      <PageHeader
        title="Projects"
        subtitle="Test reports by project. Pick one to see its launches."
        actions={
          <TextInput
            placeholder="Filter projects"
            leftSection={<IconSearch size={16} />}
            value={filter}
            onChange={(e) => setFilter(e.currentTarget.value)}
            aria-label="Filter projects"
            w={{ base: '100%', xs: 240 }}
          />
        }
      />
      <QueryState query={q}>
        {() => {
          const f = filter.trim().toLowerCase();
          const list = (q.data ?? []).filter((p) => !f || `${p.name} ${p.key} ${p.description ?? ''}`.toLowerCase().includes(f));
          if (!q.data?.length) return <EmptyState title="No projects" description="You cannot read any project yet. Ask an administrator for access." />;
          if (!list.length) return <EmptyState title="Nothing matches" description={`No project matches “${filter}”.`} />;
          return (
            <SimpleGrid cols={{ base: 1, sm: 2, lg: 3 }} spacing="md">
              {list.map((p) => (
                <ProjectCard key={p.key} project={p} />
              ))}
            </SimpleGrid>
          );
        }}
      </QueryState>
    </>
  );
}
