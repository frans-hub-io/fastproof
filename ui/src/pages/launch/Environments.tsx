import { Button, Card, Chip, Group, Select, Stack, Text, Tooltip } from '@mantine/core';
import { useState } from 'react';
import { useSearchParams } from 'react-router';
import type { EnvironmentCounts, Launch } from '../../api/types';
import { StatusBar } from '../../components/Status';
import { countsLabel, passRate } from '../../lib/counts';
import { envKey, envName, type EnvironmentScope } from './environment';

const CHIPS_UP_TO = 6;
const ROWS = 8;

function EnvRow({ e }: { e: EnvironmentCounts }) {
  const key = envKey(e);
  const c = e.counts;
  const failing = c.failed + c.broken;
  return (
    <div className="fp-env-row" data-testid="environment-row" data-env={key}>
      <Text size="xs" fw={600} className="fh-truncate" c={e.key === null || e.key === undefined ? 'dimmed' : undefined} title={envName(key)}>
        {envName(key)}
      </Text>
      <StatusBar counts={c} height={8} />
      <Tooltip label={countsLabel(c) || 'No results'}>
        <Text size="xs" c="dimmed" className="fp-env-nums" tabIndex={0} aria-label={`${envName(key)}: ${String(c.total)} results, ${countsLabel(c)}`}>
          {c.total.toLocaleString()}
          {failing > 0 && (
            <Text span size="xs" c="var(--fp-status-failed)" fw={600}>
              {' '}
              · {failing.toLocaleString()} failing
            </Text>
          )}
        </Text>
      </Tooltip>
      <Text size="xs" fw={700} ta="right" className="fp-env-rate">
        {Math.round(passRate(c) * 100)}%
      </Text>
    </div>
  );
}

/**
 * The environments the results of a launch ran in: a switcher (all, or one) that scopes the
 * summary, the tree and the result list, and the counts of each.
 */
export function EnvironmentSwitch({ launch, scope }: { launch: Launch; scope: EnvironmentScope }) {
  const [, setSp] = useSearchParams();
  const [all, setAll] = useState(false);
  if (!scope.show) return null;
  const set = (v: string | null) =>
    setSp(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (v && v !== 'all') next.set('env', v);
        else next.delete('env');
        return next;
      },
      { replace: true },
    );
  const value = scope.env ?? 'all';
  const options = [{ value: 'all', label: `All environments · ${launch.counts.total.toLocaleString()}` }, ...scope.list.map((e) => ({ value: envKey(e), label: `${envName(envKey(e))} · ${e.counts.total.toLocaleString()}` }))];
  if (scope.env && !scope.list.some((e) => envKey(e) === scope.env)) options.push({ value: scope.env, label: `${envName(scope.env)} · 0` });
  const rows = all ? scope.list : scope.list.slice(0, ROWS);
  return (
    <Card withBorder radius="lg" padding="sm" data-testid="environments">
      <Stack gap="xs">
        <Group gap="xs" justify="space-between" wrap="wrap">
          <Text size="sm" fw={600} id="fp-env-label">
            Environment
          </Text>
          {options.length - 1 > CHIPS_UP_TO && (
            <Select size="xs" w={260} maw="100%" data={options} value={value} onChange={(v) => set(v)} allowDeselect={false} searchable aria-labelledby="fp-env-label" comboboxProps={{ withinPortal: true }} data-testid="environment-select" />
          )}
        </Group>
        {options.length - 1 <= CHIPS_UP_TO && (
          <Chip.Group value={value} onChange={(v) => set(v)}>
            <Group gap={6} wrap="wrap" role="radiogroup" aria-labelledby="fp-env-label">
              {options.map((o) => (
                <Chip key={o.value} value={o.value} size="xs" variant="outline" data-testid={`environment-${o.value}`}>
                  {o.label}
                </Chip>
              ))}
            </Group>
          </Chip.Group>
        )}
        {scope.list.length > 1 && (
          <Stack gap={4} data-testid="environment-summary" aria-label="Results per environment" role="group">
            {rows.map((e) => (
              <EnvRow key={envKey(e)} e={e} />
            ))}
            {scope.list.length > ROWS && (
              <Button variant="subtle" size="compact-xs" color="gray" onClick={() => setAll((a) => !a)} style={{ alignSelf: 'flex-start' }}>
                {all ? 'Show fewer' : `Show all ${String(scope.list.length)}`}
              </Button>
            )}
          </Stack>
        )}
      </Stack>
    </Card>
  );
}
