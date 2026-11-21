import { Badge, Tooltip } from '@mantine/core';
import type { Transition } from '../../api/types-next';

const LOOK: Record<Transition, { color: string; label: string; tip: string }> = {
  new: { color: 'blue', label: 'new test', tip: 'Not in the previous launch' },
  fixed: { color: 'green', label: 'fixed', tip: 'Failed in the previous launch, passes now' },
  regressed: { color: 'red', label: 'regressed', tip: 'Passed in the previous launch, fails now' },
  malfunctioned: { color: 'orange', label: 'malfunctioned', tip: 'Passed in the previous launch, broken now' },
};

export function TransitionBadge({ transition }: { transition?: Transition | null }) {
  if (!transition) return null;
  const l = LOOK[transition];
  return (
    <Tooltip label={l.tip}>
      <Badge size="xs" variant="outline" color={l.color} radius="sm" style={{ textTransform: 'none', flex: 'none' }} data-testid="transition" data-transition={transition}>
        {l.label}
      </Badge>
    </Tooltip>
  );
}
