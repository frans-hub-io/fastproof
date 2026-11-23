import { Kbd, Modal, Stack, Switch, Table, Text } from '@mantine/core';
import { RUNNER_HELP } from './keymap';

export function HelpModal({ opened, onClose, singleKeys, onSingleKeys }: { opened: boolean; onClose: () => void; singleKeys: boolean; onSingleKeys: (on: boolean) => void }) {
  return (
    <Modal opened={opened} onClose={onClose} title="Keyboard shortcuts" size="lg" attributes={{ content: { 'data-testid': 'runner-help' } }}>
      <Stack gap="sm">
        <Switch
          label="Single-key shortcuts (p, f, j, k, …)"
          description="Turn them off if they clash with speech input or your assistive technology. Arrows, Enter and Ctrl+Enter keep working."
          checked={singleKeys}
          onChange={(e) => onSingleKeys(e.currentTarget.checked)}
          data-testid="single-keys-toggle"
        />
        <Table verticalSpacing={4} striped>
          <Table.Tbody>
            {RUNNER_HELP.map((r) => (
              <Table.Tr key={r.keys.join(' ')} style={{ opacity: !singleKeys && r.singleKey && !r.keys.some((k) => k === '↓' || k === '↑') ? 0.5 : 1 }}>
                <Table.Td w={160}>
                  {r.keys.map((k, i) => (
                    <span key={k}>
                      {i > 0 && ' or '}
                      <Kbd size="xs">{k}</Kbd>
                    </span>
                  ))}
                </Table.Td>
                <Table.Td>
                  <Text size="sm">{r.label}</Text>
                </Table.Td>
              </Table.Tr>
            ))}
          </Table.Tbody>
        </Table>
      </Stack>
    </Modal>
  );
}
