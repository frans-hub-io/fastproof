import { Select, type SelectProps } from '@mantine/core';
import { useMemo } from 'react';
import { launchOptionLabel, useLaunchList } from '../lib/launches';

/** A searchable picker of the project's recent launches (first page of the launch list). */
export function LaunchSelect({ project, value, onChange, exclude, ...rest }: { project: string; value: string | null; onChange: (id: string | null) => void; exclude?: string } & Omit<SelectProps, 'data' | 'value' | 'onChange'>) {
  const { launches, pending } = useLaunchList(project);
  const data = useMemo(() => launches.filter((l) => l.id !== exclude).map((l) => ({ value: l.id, label: launchOptionLabel(l) })), [launches, exclude]);
  return <Select searchable data={data} value={value} onChange={onChange} nothingFoundMessage={pending ? 'Loading…' : 'No launches'} comboboxProps={{ withinPortal: true }} {...rest} />;
}
