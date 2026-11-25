import { Badge, Menu, Text, UnstyledButton } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconChevronDown } from '@tabler/icons-react';
import { errorMessage } from '../../api/client';
import { MISSING_LABEL, transitionRefusal, useCaseWorkflow, useReviewEvents, useReviewThread, useTransitionCase } from '../../api/reviews';
import { isConflict, useCase } from '../../api/tm';

function Dot({ color }: { color: string | null | undefined }) {
  return <span aria-hidden style={{ display: 'inline-block', width: 8, height: 8, borderRadius: '50%', background: color ?? 'var(--fp-tm-case-draft)', border: '1px solid var(--fh-ui-hairline)' }} />;
}

function reason(missing: string[]): string {
  return missing.map((m) => MISSING_LABEL[m] ?? m).join(', ');
}

/** The workflow state of a case in its header; a menu of the moves from it. Renders nothing without a workflow. */
export function StateChip({ projectKey, caseNumber }: { projectKey: string; caseNumber: number }) {
  const canWrite = useCan('cases:write', projectKey);
  const wf = useCaseWorkflow(projectKey);
  const c = useCase(projectKey, caseNumber);
  const configured = wf.data?.configured === true;
  const thread = useReviewThread(projectKey, 'case', caseNumber, configured);
  const transition = useTransitionCase(projectKey, caseNumber);
  useReviewEvents(projectKey);

  const caseStatus = c.data?.value.status;
  const statuses = wf.data?.workflow.statuses ?? [];
  const derived = caseStatus ? (statuses.find((s) => s.key === caseStatus) ?? statuses.find((s) => s.category === caseStatus))?.key : undefined;
  const state = c.data?.value.state ?? derived ?? null;
  if (!configured || !state || !wf.data) return null;
  const status = statuses.find((s) => s.key === state);
  const name = status?.name ?? state;
  const etag = c.data?.etag;
  const moves = thread.data?.thread.transitions ?? [];

  const label = (
    <Badge variant="outline" color="gray" size="sm" radius="sm" tt="none" leftSection={<Dot color={status?.color} />} rightSection={canWrite && moves.length > 0 ? <IconChevronDown size={11} aria-hidden /> : undefined} data-testid="state-chip" data-state={state} styles={{ root: { flex: 'none', cursor: canWrite && moves.length > 0 ? 'pointer' : 'default' } }}>
      {name}
    </Badge>
  );

  if (!canWrite || moves.length === 0) return label;

  const go = (to: string, toName: string) => {
    if (!etag) return;
    transition.mutate(
      { to, etag },
      {
        onSuccess: () => notifications.show({ color: 'green', title: 'State changed', message: `Moved to ${toName}` }),
        onError: (e) => {
          const refusal = transitionRefusal(e);
          void thread.refetch();
          void c.refetch();
          notifications.show({ color: 'red', title: 'Not moved', message: refusal ? `${errorMessage(e)}${refusal.missing.length ? ` (${reason(refusal.missing)})` : ''}` : isConflict(e) ? 'The case changed meanwhile; its state was reloaded.' : errorMessage(e) });
        },
      },
    );
  };

  return (
    <Menu withinPortal position="bottom-start" shadow="md" width={280}>
      <Menu.Target>
        <UnstyledButton aria-label={`State: ${name}. Change state`} data-testid="state-menu" style={{ display: 'inline-flex', borderRadius: 4 }}>
          {label}
        </UnstyledButton>
      </Menu.Target>
      <Menu.Dropdown data-testid="state-menu-list">
        <Menu.Label>Move to</Menu.Label>
        {moves.map((m) => (
          <Menu.Item key={m.to} disabled={!m.allowed || transition.isPending} onClick={() => go(m.to, m.name)} data-testid="state-option" data-state={m.to} data-allowed={m.allowed} aria-describedby={m.allowed ? undefined : `why-${m.to}`}>
            <Text size="sm">{m.name}</Text>
            {!m.allowed && (
              <Text size="xs" c="dimmed" id={`why-${m.to}`} data-testid="state-why">
                {reason(m.missing)}
              </Text>
            )}
          </Menu.Item>
        ))}
      </Menu.Dropdown>
    </Menu>
  );
}
