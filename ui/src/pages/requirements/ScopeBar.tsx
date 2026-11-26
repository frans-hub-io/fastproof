import { Button, Group, NumberInput, Select, SegmentedControl, Stack, TextInput } from '@mantine/core';
import { useState, type SyntheticEvent } from 'react';
import { useManagedPlans } from '../../api/tm';
import { launchIdList, scopeReady, type CoverageScope } from '../../api/requirements';

const KINDS = [
  { value: 'default', label: 'Last results' },
  { value: 'plan', label: 'Plan' },
  { value: 'launches', label: 'Launches' },
  { value: 'window', label: 'Window' },
];

const SCOPE_HINT: Record<CoverageScope['kind'], string> = {
  default: 'Each case counts with its newest result.',
  plan: 'The results of the newest launch of a plan, or of one launch you name.',
  launches: 'Only the results of the launches you list (at most 50).',
  window: 'Newest results of the last days, optionally for one branch and environment.',
};

/** Picks what coverage counts; the choice is applied (and kept in the URL) with the button. */
export function ScopeBar({ project, scope, onApply }: { project: string; scope: CoverageScope; onApply: (s: CoverageScope) => void }) {
  const [draft, setDraft] = useState<CoverageScope>(scope);
  const plans = useManagedPlans(project, {});
  const patch = (p: Partial<CoverageScope>) => setDraft((d) => ({ ...d, ...p }));
  const tooMany = launchIdList(draft.launchIds).length > 50;
  const ready = scopeReady(draft) && !tooMany;
  const submit = (e: SyntheticEvent) => {
    e.preventDefault();
    if (ready) onApply(draft);
  };
  return (
    <form onSubmit={submit} data-testid="scope-bar">
      <Stack gap="xs">
        <SegmentedControl
          aria-label="Scope"
          size="xs"
          data={KINDS}
          value={draft.kind}
          onChange={(v) => setDraft(v === 'window' ? { kind: 'window', days: 30 } : { kind: v as CoverageScope['kind'] })}
          styles={{ root: { maxWidth: '100%', overflowX: 'auto' } }}
          data-testid="scope-kind"
        />
        <Group gap="sm" align="flex-end" wrap="wrap">
          {draft.kind === 'plan' && (
            <>
              {plans.data && plans.data.length > 0 ? (
                <Select label="Plan" searchable allowDeselect={false} placeholder="Choose a plan" data={plans.data.map((p) => ({ value: p.id, label: p.name }))} value={draft.planId ?? null} onChange={(v) => patch({ planId: v ?? undefined })} w={{ base: '100%', xs: 260 }} data-testid="scope-plan" />
              ) : (
                <TextInput label="Plan id" value={draft.planId ?? ''} onChange={(e) => patch({ planId: e.currentTarget.value.trim() || undefined })} w={{ base: '100%', xs: 280 }} data-testid="scope-plan" />
              )}
              <TextInput label="Launch id" description="Optional" value={draft.launchId ?? ''} onChange={(e) => patch({ launchId: e.currentTarget.value.trim() || undefined })} w={{ base: '100%', xs: 280 }} data-testid="scope-launch" />
            </>
          )}
          {draft.kind === 'launches' && <TextInput label="Launch ids" description="Separated by commas" value={draft.launchIds ?? ''} onChange={(e) => patch({ launchIds: e.currentTarget.value })} error={tooMany ? 'At most 50 launches' : undefined} style={{ flex: '1 1 280px' }} data-testid="scope-launches" />}
          {draft.kind === 'window' && (
            <>
              <NumberInput label="Days" min={1} max={365} allowDecimal={false} clampBehavior="strict" value={draft.days ?? 30} onChange={(v) => patch({ days: typeof v === 'number' ? v : 30 })} w={90} data-testid="scope-days" />
              <TextInput label="Branch" placeholder="any" value={draft.branch ?? ''} onChange={(e) => patch({ branch: e.currentTarget.value.trim() || undefined })} w={{ base: '45%', xs: 150 }} data-testid="scope-branch" />
              <TextInput label="Environment" placeholder="any" value={draft.environment ?? ''} onChange={(e) => patch({ environment: e.currentTarget.value.trim() || undefined })} w={{ base: '45%', xs: 150 }} data-testid="scope-environment" />
            </>
          )}
          <Button type="submit" size="sm" variant="default" disabled={!ready} data-testid="scope-apply">
            Apply
          </Button>
        </Group>
        <span style={{ fontSize: 12, color: 'var(--mantine-color-dimmed)' }}>{SCOPE_HINT[draft.kind]}</span>
      </Stack>
    </form>
  );
}
