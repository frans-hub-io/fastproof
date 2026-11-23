import { Autocomplete, type AutocompleteProps } from '@mantine/core';
import { useDebouncedValue } from '@mantine/hooks';
import { usePeople, type Person } from '../../api/tm';
import { isKnownPerson } from '../../lib/tm/ui';

function optionLabel(p: Person): string {
  return p.display && p.display !== p.principal ? `${p.display} (${p.principal})` : p.principal;
}

/**
 * An assignee box: suggestions come from `GET /people?q=` (people bound to the project, members of bound groups who
 * have signed in, and assignees or executors seen in its runs). Free text is allowed; a name the server does not know is marked so typos are visible.
 */
export function PersonInput({ project, value, onChange, warnUnknown = true, ...rest }: { project: string; value: string; onChange: (v: string) => void; warnUnknown?: boolean } & Omit<AutocompleteProps, 'value' | 'onChange' | 'data'>) {
  const [q] = useDebouncedValue(value.trim(), 200);
  const people = usePeople(project, q);
  const labels = new Map((people.data ?? []).map((p) => [p.principal, optionLabel(p)] as const));
  const data = [...labels.keys()];
  const settled = q === value.trim() && !people.isFetching;
  const unknown = warnUnknown && settled && !!people.data && !isKnownPerson(people.data, value);
  return (
    <Autocomplete
      {...rest}
      value={value}
      onChange={onChange}
      data={data}
      filter={({ options }) => options}
      renderOption={({ option }) => labels.get(option.value) ?? option.value}
      comboboxProps={{ withinPortal: true, ...rest.comboboxProps }}
      description={unknown ? 'Not a known user of this project; check the spelling. Members of a bound group are known once they have signed in.' : rest.description}
      styles={unknown ? { description: { color: 'var(--fh-semantic-warning-fg)' } } : undefined}
      data-unknown={unknown || undefined}
    />
  );
}
