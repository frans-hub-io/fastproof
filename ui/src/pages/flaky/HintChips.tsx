import { Badge, Group, Text, Tooltip, UnstyledButton } from '@mantine/core';
import { useState } from 'react';
import { CONFIDENCE_COLOR, HINT_KIND_HELP, HINT_KIND_LABEL } from '../../api/flakyHints';
import type { FlakyTest } from '../../api/types-next';
import { HintPanel } from './HintPanel';

/** Chips shown in the list, at most. */
const SHOWN = 2;

/** Likely causes of a flaky test: chips that open the evidence panel. */
export function HintChips({ project, item }: { project: string; item: FlakyTest }) {
  const [open, setOpen] = useState(false);
  const hints = item.hints;
  const infra = item.infraShare ?? 0;
  if (hints.length === 0 && infra === 0) {
    return (
      <Text size="xs" c="dimmed" data-testid="hint-none">
        –
      </Text>
    );
  }
  return (
    <>
      <UnstyledButton onClick={() => setOpen(true)} aria-label={`Likely causes of ${item.name}`} data-testid="hint-chips" style={{ display: 'block', maxWidth: '100%' }}>
        <Group gap={4} wrap="wrap">
          {hints.slice(0, SHOWN).map((h) => (
            <Tooltip key={h.kind} label={`${HINT_KIND_HELP[h.kind]} Confidence: ${h.confidence}.`} multiline w={260}>
              <Badge size="sm" variant="light" color={CONFIDENCE_COLOR[h.confidence]} style={{ textTransform: 'none', cursor: 'pointer' }} data-testid="hint-chip" data-kind={h.kind}>
                {HINT_KIND_LABEL[h.kind]}
              </Badge>
            </Tooltip>
          ))}
          {hints.length > SHOWN && (
            <Badge size="sm" variant="outline" color="gray" style={{ textTransform: 'none', cursor: 'pointer' }}>
              +{hints.length - SHOWN}
            </Badge>
          )}
          {infra > 0 && (
            <Tooltip label="Share of the failures with infrastructure evidence; they still count in the flakiness score">
              <Text span size="xs" c="dimmed" data-testid="hint-infra-share">
                infra {Math.round(infra * 100)}%
              </Text>
            </Tooltip>
          )}
        </Group>
      </UnstyledButton>
      {open && <HintPanel project={project} item={item} onClose={() => setOpen(false)} />}
    </>
  );
}
