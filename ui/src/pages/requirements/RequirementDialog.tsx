import { Alert, Button, Group, Modal, NumberInput, Select, Stack, Text, TextInput, Textarea } from '@mantine/core';
import { useState, type SyntheticEvent } from 'react';
import { isConflict } from '../../api/tm';
import {
  REQUIREMENT_STATUSES,
  requirementError,
  useCreateRequirement,
  usePatchRequirement,
  type Requirement,
  type RequirementPatch,
  type RequirementStatus,
  type RequirementWrite,
} from '../../api/requirements';
import { isHttpUrl } from '../../lib/format';

interface Props {
  project: string;
  onClose: () => void;
  onSaved: (r: Requirement) => void;
  edit?: { requirement: Requirement; etag: string };
  parentNumber?: number | null;
  onReload?: () => void;
}

const parentOf = (r: Requirement): number | null => (r.path.length > 1 ? (r.path[r.path.length - 2]?.number ?? null) : null);

export function RequirementDialog({ project, onClose, onSaved, edit, parentNumber, onReload }: Props) {
  const current = edit?.requirement;
  const create = useCreateRequirement(project);
  const patch = usePatchRequirement(project, current?.number ?? 0);
  const [title, setTitle] = useState(current?.title ?? '');
  const [type, setType] = useState(current?.type ?? 'story');
  const [status, setStatus] = useState<RequirementStatus>(current?.status ?? 'draft');
  const [parent, setParent] = useState<number | string>(current ? (parentOf(current) ?? '') : (parentNumber ?? ''));
  const [description, setDescription] = useState(current?.description ?? '');
  const [issueKey, setIssueKey] = useState('');
  const [url, setUrl] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const parentValue = parent === '' ? null : Number(parent);
  const problem = !title.trim() ? 'A title is required.' : url && !isHttpUrl(url) ? 'The URL must start with http:// or https://.' : null;
  const busy = create.isPending || patch.isPending;

  const submit = (e: SyntheticEvent) => {
    e.preventDefault();
    if (problem) return;
    setError(null);
    if (edit) {
      const cur = edit.requirement;
      const body: RequirementPatch = {};
      if (title.trim() !== cur.title) body.title = title.trim();
      if (description !== cur.description) body.description = description;
      if (type.trim() !== cur.type) body.type = type.trim();
      if (status !== cur.status) body.status = status;
      if (parentValue !== parentOf(cur)) body.parentNumber = parentValue;
      if (Object.keys(body).length === 0) {
        onClose();
        return;
      }
      patch.mutate(
        { body, etag: edit.etag },
        {
          onSuccess: (v) => onSaved(v.value),
          onError: (err) => (isConflict(err) ? setStale(true) : setError(requirementError(err))),
        },
      );
      return;
    }
    const body: RequirementWrite = { title: title.trim(), type: type.trim() || undefined, status, description, parentNumber: parentValue };
    if (issueKey.trim()) body.issueKey = issueKey.trim();
    if (url.trim()) body.url = url.trim();
    create.mutate(body, { onSuccess: onSaved, onError: (err) => setError(requirementError(err)) });
  };

  return (
    <Modal opened onClose={onClose} title={current ? `Edit REQ-${String(current.number)}` : 'New requirement'} size="lg">
      <form onSubmit={submit}>
        <Stack gap="sm">
          {stale && (
            <Alert color="yellow" title="Changed by someone else" data-testid="requirement-stale">
              <Group justify="space-between" gap="xs">
                <Text size="sm">This requirement changed after you opened it. Reload to see the latest version, then edit again.</Text>
                <Button
                  size="xs"
                  variant="default"
                  onClick={() => {
                    onReload?.();
                    onClose();
                  }}
                >
                  Reload
                </Button>
              </Group>
            </Alert>
          )}
          <TextInput label="Title" required data-autofocus value={title} onChange={(e) => setTitle(e.currentTarget.value)} maxLength={300} data-testid="requirement-title" />
          <Group grow align="flex-start">
            <TextInput label="Type" value={type} onChange={(e) => setType(e.currentTarget.value)} maxLength={64} data-testid="requirement-type" />
            <Select label="Status" allowDeselect={false} data={REQUIREMENT_STATUSES.map((s) => ({ value: s, label: s }))} value={status} onChange={(v) => v && setStatus(v)} data-testid="requirement-status" />
          </Group>
          <NumberInput label="Parent requirement number" description="Leave empty for the top level." min={1} allowDecimal={false} hideControls value={parent} onChange={setParent} data-testid="requirement-parent" />
          <Textarea label="Description" description="Markdown." autosize minRows={3} maxRows={10} value={description} onChange={(e) => setDescription(e.currentTarget.value)} data-testid="requirement-description" />
          {!current && (
            <Group grow align="flex-start">
              <TextInput label="Issue key" placeholder="SHOP-210" value={issueKey} onChange={(e) => setIssueKey(e.currentTarget.value)} data-testid="requirement-issue-key" />
              <TextInput label="Issue URL" placeholder="https://issues.example.com/browse/SHOP-210" value={url} onChange={(e) => setUrl(e.currentTarget.value)} data-testid="requirement-url" />
            </Group>
          )}
          {error && (
            <Alert color="red" data-testid="requirement-error">
              {error}
            </Alert>
          )}
          <Group justify="space-between">
            <Text size="xs" c="dimmed">
              {problem ?? ' '}
            </Text>
            <Group>
              <Button variant="default" onClick={onClose}>
                Cancel
              </Button>
              <Button type="submit" disabled={!!problem || stale} loading={busy} data-testid="requirement-save">
                {current ? 'Save' : 'Create'}
              </Button>
            </Group>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}
