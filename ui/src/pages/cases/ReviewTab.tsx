import { Alert, Badge, Button, Card, Group, Stack, Text, Textarea } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconArrowRight, IconCheck, IconInfoCircle, IconMessageX } from '@tabler/icons-react';
import { useEffect, useMemo, useState } from 'react';
import { Link } from 'react-router';
import { ApiError, errorMessage } from '../../api/client';
import { REVIEW_STATE_COLOR, REVIEW_STATE_LABEL, WHY_NOT, useReviewThread, useVote, versionMoved, type ReviewDecision, type ReviewThread } from '../../api/reviews';
import { isConflict, useCase, useCaseFields, useCaseFolders, useCaseVersion } from '../../api/tm';
import { QueryState } from '../../components/QueryState';
import { formatDateTime, formatRelative } from '../../lib/format';
import { folderPathLabel } from '../../lib/tm/ui';
import { EMPTY_SNAPSHOT, bytesOf } from '../../lib/tm/reviewUtil';
import { diffSnapshots } from '../../lib/tm/versionDiff';
import { VersionDiffView } from './VersionDiffView';

const MAX_COMMENT_BYTES = 4000;

export function StateBadge({ state }: { state: ReviewThread['reviewState'] }) {
  if (!state) return null;
  return (
    <Badge variant="light" color={REVIEW_STATE_COLOR[state]} size="sm" radius="sm" tt="none" data-testid="review-state" data-state={state} styles={{ root: { flex: 'none', maxWidth: 'none' }, label: { overflow: 'visible', textOverflow: 'clip' } }}>
      {REVIEW_STATE_LABEL[state]}
    </Badge>
  );
}

/** The diff between the approved version and the head of a case (the head against an empty case when none is approved). */
export function ReviewDiff({ project, number, approved, head }: { project: string; number: number; approved: number | null; head: number }) {
  const fields = useCaseFields(project);
  const folders = useCaseFolders(project);
  const a = useCaseVersion(project, number, approved);
  const b = useCaseVersion(project, number, head);
  const labels = useMemo(() => Object.fromEntries((fields.data ?? []).map((f) => [f.key, f.name])), [fields.data]);
  const diff = b.data && (approved === null || a.data) ? diffSnapshots(a.data?.snapshot ?? EMPTY_SNAPSHOT, b.data.snapshot, labels, (id) => folderPathLabel(folders.data, id)) : null;
  const beforeLabel = approved === null ? 'New case' : `v${String(approved)} (approved)`;
  const afterLabel = `v${String(head)} (head)`;
  if (b.isError || a.isError) {
    return (
      <Text size="sm" c="red">
        The versions could not be loaded.
      </Text>
    );
  }
  if (!diff) {
    return (
      <Text size="sm" c="dimmed">
        Loading the changes…
      </Text>
    );
  }
  return <VersionDiffView diff={diff} beforeLabel={beforeLabel} afterLabel={afterLabel} showSame={approved === null} />;
}

function VoteList({ thread }: { thread: ReviewThread }) {
  if (thread.votes.length === 0) {
    return (
      <Text size="sm" c="dimmed" data-testid="votes-empty">
        No votes yet.
      </Text>
    );
  }
  return (
    <Stack gap={6} component="ul" m={0} p={0} style={{ listStyle: 'none' }} data-testid="votes">
      {thread.votes.map((v) => (
        <li key={`${v.reviewer}-${String(v.version)}-${v.createdAt}`} data-testid="vote" data-decision={v.decision}>
          <Card withBorder radius="md" padding="xs">
            <Group gap={6} wrap="wrap">
              <Text size="sm" fw={600}>
                {v.reviewer}
              </Text>
              <Badge size="xs" variant="light" radius="sm" tt="none" color={v.decision === 'approve' ? 'teal' : 'red'}>
                {v.decision === 'approve' ? 'approved' : 'requested changes'}
              </Badge>
              <Text size="xs" c="dimmed">
                v{v.version} ·{' '}
                <span title={formatDateTime(v.createdAt)}>{formatRelative(v.createdAt)}</span>
              </Text>
            </Group>
            {v.comment && (
              <Text size="sm" mt={4} style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-word' }} data-testid="vote-comment">
                {v.comment}
              </Text>
            )}
          </Card>
        </li>
      ))}
    </Stack>
  );
}

function ReviewOff({ projectKey, canSettings }: { projectKey: string; canSettings: boolean }) {
  return (
    <Alert variant="light" color="gray" icon={<IconInfoCircle size={16} />} title="Review is off for this project" data-testid="review-off">
      <Text size="sm">Versions of cases are not reviewed.</Text>
      {canSettings && (
        <Text size="sm">
          Turn it on in the{' '}
          <Link to={`/p/${projectKey}/settings?tab=workflow`} data-testid="review-off-link">
            Workflow settings
          </Link>
          .
        </Text>
      )}
    </Alert>
  );
}

export function ReviewTab({ projectKey, caseNumber }: { projectKey: string; caseNumber: number }) {
  const canReview = useCan('cases:review', projectKey);
  const canSettings = useCan('settings:write', projectKey);
  const query = useReviewThread(projectKey, 'case', caseNumber);
  const vote = useVote(projectKey, 'case', caseNumber);
  const revision = useCase(projectKey, caseNumber).data?.value.revision;
  const refetch = query.refetch;
  useEffect(() => {
    if (revision !== undefined) void refetch();
  }, [revision, refetch]);
  const [comment, setComment] = useState('');
  const [note, setNote] = useState<string | null>(null);
  const tooLong = bytesOf(comment) > MAX_COMMENT_BYTES;

  const errorOff = query.error instanceof ApiError && query.error.code === 'review_not_enabled';

  const decide = (thread: ReviewThread, etag: string | null, decision: ReviewDecision) => {
    setNote(null);
    vote.mutate(
      { body: { version: thread.headVersion, decision, ...(comment.trim() ? { comment: comment.trim() } : {}) }, etag },
      {
        onSuccess: () => {
          setComment('');
          notifications.show({ color: 'green', title: decision === 'approve' ? 'Approved' : 'Changes requested', message: `Your decision on v${String(thread.headVersion)} is saved.` });
        },
        onError: (e) => {
          if (versionMoved(e) || isConflict(e)) {
            void query.refetch();
            setNote('The case changed while you were reviewing. The latest version is shown now; read the changes again before you decide.');
            notifications.show({ color: 'yellow', title: 'The case changed', message: 'A newer version exists; review it again.' });
            return;
          }
          void query.refetch();
          notifications.show({ color: 'red', title: 'Not saved', message: errorMessage(e) });
        },
      },
    );
  };

  if (errorOff) return <ReviewOff projectKey={projectKey} canSettings={canSettings} />;

  return (
    <QueryState query={query}>
      {() => {
        const read = query.data;
        if (!read) return null;
        const t = read.thread;
        if (t.reviewState == null && t.whyNot === 'review_not_enabled') return <ReviewOff projectKey={projectKey} canSettings={canSettings} />;
        return (
          <Stack gap="md" data-testid="review-tab">
            <Group gap="sm" wrap="wrap" align="center">
              <StateBadge state={t.reviewState} />
              <Text size="sm" className="fp-mono" data-testid="review-versions">
                {t.approvedVersion != null ? `v${String(t.approvedVersion)}` : 'no approved version'} <IconArrowRight size={12} aria-label="to" style={{ verticalAlign: 'middle' }} /> v{t.headVersion}
              </Text>
              <Text size="sm" data-testid="review-approvals">
                Approvals {t.approvals} / {t.required}
              </Text>
              {t.author && (
                <Text size="xs" c="dimmed">
                  by {t.author}
                </Text>
              )}
            </Group>
            {note && (
              <Alert color="yellow" variant="light" role="status" data-testid="review-moved">
                {note}
              </Alert>
            )}
            <Stack gap="xs">
              <Text size="sm" fw={600} component="h3" m={0}>
                {t.approvedVersion != null ? `Changes since v${String(t.approvedVersion)}` : 'New case'}
              </Text>
              <ReviewDiff project={projectKey} number={caseNumber} approved={t.approvedVersion ?? null} head={t.headVersion} />
            </Stack>
            <Stack gap="xs">
              <Text size="sm" fw={600} component="h3" m={0}>
                Votes
              </Text>
              <VoteList thread={t} />
            </Stack>
            <Stack gap="xs" data-testid="review-form">
              <Textarea label="Comment" description="Plain text, up to 4000 bytes." value={comment} onChange={(e) => setComment(e.currentTarget.value)} minRows={2} autosize disabled={!t.canVote} error={tooLong ? 'The comment is longer than 4000 bytes' : undefined} data-testid="review-comment" />
              <Group gap="xs">
                <Button size="xs" color="teal" leftSection={<IconCheck size={14} />} disabled={!t.canVote || tooLong || !canReview} loading={vote.isPending} onClick={() => decide(t, read.etag, 'approve')} data-testid="review-approve">
                  Approve
                </Button>
                <Button size="xs" color="red" variant="light" leftSection={<IconMessageX size={14} />} disabled={!t.canVote || tooLong || !canReview} loading={vote.isPending} onClick={() => decide(t, read.etag, 'request_changes')} data-testid="review-request">
                  Request changes
                </Button>
              </Group>
              {!t.canVote && (
                <Text size="sm" c="dimmed" data-testid="review-why">
                  {WHY_NOT[t.whyNot ?? ''] ?? t.whyNot ?? 'You cannot vote on this version.'}
                </Text>
              )}
            </Stack>
          </Stack>
        );
      }}
    </QueryState>
  );
}
