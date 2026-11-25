import { Stack, TextInput } from '@mantine/core';
import type { SharedDraft } from '../../lib/tm/shared';
import { MarkdownField } from '../cases/MarkdownField';
import { StepsEditor } from '../cases/StepsEditor';

/** Name, description and steps of a shared step (two levels of steps, no attachments, no shared steps inside). */
export function SharedStepForm({ project, draft, onChange, onAnnounce }: { project: string; draft: SharedDraft; onChange: (d: SharedDraft) => void; onAnnounce: (m: string) => void }) {
  return (
    <Stack gap="sm" data-testid="shared-form">
      <TextInput label="Name" value={draft.name} onChange={(e) => onChange({ ...draft, name: e.currentTarget.value })} required maxLength={200} data-autofocus data-testid="shared-name" />
      <MarkdownField project={project} caseNumber={null} label="Description" value={draft.description} onChange={(description) => onChange({ ...draft, description })} minRows={2} testId="shared-description" inputId="shared-description" />
      <StepsEditor project={project} caseNumber={null} steps={draft.steps} onChange={(steps) => onChange({ ...draft, steps })} onAnnounce={onAnnounce} allowShared={false} maxDepth={2} attachments={false} />
    </Stack>
  );
}
