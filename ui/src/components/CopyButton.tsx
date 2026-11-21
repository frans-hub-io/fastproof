import { ActionIcon, CopyButton as MantineCopy, Tooltip } from '@mantine/core';
import { IconCheck, IconCopy } from '@tabler/icons-react';

export function CopyValue({ value, label = 'Copy' }: { value: string; label?: string }) {
  return (
    <MantineCopy value={value} timeout={1500}>
      {({ copied, copy }) => (
        <Tooltip label={copied ? 'Copied' : label}>
          <ActionIcon variant="subtle" size="sm" onClick={copy} aria-label={label}>
            {copied ? <IconCheck size={14} /> : <IconCopy size={14} />}
          </ActionIcon>
        </Tooltip>
      )}
    </MantineCopy>
  );
}
