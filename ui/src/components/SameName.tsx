import { Text, Tooltip } from '@mantine/core';

/** A dimmed id after a name that other rows of the list share. */
export function SameName({ hint }: { hint: string | undefined }) {
  if (!hint) return null;
  return (
    <Tooltip label="Another test in this list has the same name; open it to see its suite and parameters">
      <Text span size="xs" c="dimmed" className="fp-mono" ml={6} data-testid="same-name-hint">
        {hint}
      </Text>
    </Tooltip>
  );
}
