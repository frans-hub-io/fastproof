import { Alert, Group, Stack, Table, Text } from '@mantine/core';
import { useParams, useSearchParams } from 'react-router';
import { useMemo } from 'react';
import { useProject } from '../api/hooks';
import { scopeFromParams, scopeReady, scopeToParams, useCoverage, useRequirementEvents, type CoverageScope } from '../api/requirements';
import { PageHeader } from '../components/PageHeader';
import { CoverageBranch } from './requirements/CoverageRows';
import { ExportMenu } from './requirements/ExportMenu';
import { ScopeBar } from './requirements/ScopeBar';

function scopeSummary(s: CoverageScope, launches: number): string {
  const n = `${String(launches)} launch${launches === 1 ? '' : 'es'}`;
  switch (s.kind) {
    case 'plan':
      return `Results of ${n} of the plan`;
    case 'launches':
      return `Results of ${n}`;
    case 'window':
      return `Newest results of the last ${String(s.days ?? 30)} days${s.branch ? `, branch ${s.branch}` : ''}${s.environment ? `, environment ${s.environment}` : ''} (${n})`;
    default:
      return `Newest result of each case (${n})`;
  }
}

export default function Coverage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const [params, setParams] = useSearchParams();
  const scope = useMemo(() => scopeFromParams(params), [params]);
  const first = useCoverage(key, scope, null);
  const launches = first.data?.pages[0]?.scope.launches ?? 0;
  useRequirementEvents(key);

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Coverage' }]}
        title="Coverage"
        subtitle="How well the requirements are covered by passing cases. Each row counts the cases of everything under it."
        actions={<ExportMenu project={key} scope={scope} disabled={!scopeReady(scope)} />}
      />
      <Stack gap="sm">
        <ScopeBar key={params.toString()} project={key} scope={scope} onApply={(s) => setParams(scopeToParams(s), { replace: false })} />
        {scopeReady(scope) ? (
          <Text size="sm" c="dimmed" data-testid="scope-summary">
            {scopeSummary(scope, launches)}
          </Text>
        ) : (
          <Alert color="blue">Choose what to count, then press Apply.</Alert>
        )}
        {scopeReady(scope) && (
          <Table.ScrollContainer minWidth={300}>
            <Table verticalSpacing={6} fz="sm" data-testid="coverage-table">
              <Table.Thead>
                <Table.Tr>
                  <Table.Th>Requirement</Table.Th>
                  <Table.Th>Coverage</Table.Th>
                  <Table.Th visibleFrom="sm">Cases</Table.Th>
                  <Table.Th w={40}>
                    <Group gap={0}>
                      <Text span className="fp-sr-only">
                        Show cases
                      </Text>
                    </Group>
                  </Table.Th>
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                <CoverageBranch project={key} scope={scope} parent={null} depth={0} />
              </Table.Tbody>
            </Table>
          </Table.ScrollContainer>
        )}
      </Stack>
    </>
  );
}
