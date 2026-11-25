import { ActionIcon, Button, Group, Stack, TagsInput, Text, TextInput, Tooltip } from '@mantine/core';
import { IconPlus, IconTrash } from '@tabler/icons-react';
import type { RerunSettings } from '../../api/types-next';
import { MAX_REFS, MAX_VALUES, MAX_VARIABLES, rerunPlanErrors, type Variable } from '../../lib/tm/rerunPlan';

export function RerunPlanFields({ rr, onChange }: { rr: RerunSettings; onChange: (next: RerunSettings) => void }) {
  const errors = rerunPlanErrors(rr);
  const vars = rr.variables ?? [];
  const setVar = (i: number, patch: Partial<Variable>) => onChange({ ...rr, variables: vars.map((v, j) => (j === i ? { ...v, ...patch } : v)) });
  return (
    <Stack gap="xs" data-testid="rerun-plan-fields">
      {rr.provider !== 'webhook' && (
        <TagsInput
          label="Branches and tags a plan run may use"
          description={`Patterns, at most ${String(MAX_REFS)}: * stays within one path segment, ** crosses /. The ref above is always allowed; with no patterns only it is.`}
          placeholder="release/*"
          value={rr.refs ?? []}
          onChange={(v) => onChange({ ...rr, refs: v.map((x) => x.trim()).filter(Boolean) })}
          error={errors.refs}
          maxTags={MAX_REFS}
          data-testid="rerun-refs"
        />
      )}
      <Text size="sm" fw={600}>
        Variables a plan run may set
      </Text>
      <Text size="xs" c="dimmed">
        Only these names reach the CI, by name; a variable with values accepts only those, one without accepts any text of 1 to 256 bytes. The default is sent when a run does not set the variable. FASTPROOF_ names are reserved.
      </Text>
      {vars.map((v, i) => (
        <Stack key={i} gap={4} data-testid="rerun-variable">
          <Group gap="xs" align="flex-start" wrap="wrap">
            <TextInput label={i === 0 ? 'Name' : undefined} aria-label="Variable name" placeholder="BROWSER" value={v.name} onChange={(e) => setVar(i, { name: e.currentTarget.value.trim().toUpperCase() })} w={170} data-testid="rerun-variable-name" />
            <TagsInput
              label={i === 0 ? 'Allowed values' : undefined}
              aria-label="Allowed values"
              placeholder="any value"
              value={v.values ?? []}
              onChange={(vals) => setVar(i, { values: vals, default: v.default && vals.length > 0 && !vals.includes(v.default) ? undefined : v.default })}
              maxTags={MAX_VALUES}
              style={{ flex: '1 1 200px', minWidth: 0 }}
              data-testid="rerun-variable-values"
            />
            <TextInput label={i === 0 ? 'Default' : undefined} aria-label="Default value" placeholder="none" value={v.default ?? ''} onChange={(e) => setVar(i, { default: e.currentTarget.value || undefined })} w={140} data-testid="rerun-variable-default" />
            <TextInput label={i === 0 ? 'Description' : undefined} aria-label="Description" placeholder="shown in the start dialog" value={v.description ?? ''} onChange={(e) => setVar(i, { description: e.currentTarget.value || undefined })} maxLength={200} style={{ flex: '1 1 160px', minWidth: 0 }} />
            <Tooltip label="Remove the variable">
              <ActionIcon variant="subtle" color="red" mt={i === 0 ? 24 : 0} aria-label={`Remove variable ${v.name || String(i + 1)}`} onClick={() => onChange({ ...rr, variables: vars.filter((_, j) => j !== i) })}>
                <IconTrash size={16} />
              </ActionIcon>
            </Tooltip>
          </Group>
          {errors.variables[i] && (
            <Text size="xs" c="red">
              {errors.variables[i]}
            </Text>
          )}
        </Stack>
      ))}
      <Group>
        <Button size="xs" variant="default" leftSection={<IconPlus size={14} />} disabled={vars.length >= MAX_VARIABLES} onClick={() => onChange({ ...rr, variables: [...vars, { name: '' }] })} data-testid="rerun-variable-add">
          Add a variable
        </Button>
      </Group>
    </Stack>
  );
}
