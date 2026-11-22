import { Badge, Button, Checkbox, CloseButton, Group, NumberInput, Popover, ScrollArea, Stack, Text, TextInput } from '@mantine/core';
import { useDebouncedValue } from '@mantine/hooks';
import { IconFilter, IconX } from '@tabler/icons-react';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { useLaunchFacets } from '../../api/hooks';
import { useDefectTypeMap } from '../../api/stageC';
import type { Launch, LaunchFacets } from '../../api/types';
import type { DefectType } from '../../api/types-next';
import { useLatestSearchParams } from '../../lib/urlState';
import { extraCount, type FilterParam, listValues, NONE_VALUE, parseExplorerParams, toggleValue } from '../../lib/explorer';

const LABELS: Record<string, string> = { severity: 'Severity', owner: 'Owner', tag: 'Tag' };
const ATTACHMENTS: Record<string, string> = { trace: 'Trace', video: 'Video', screenshot: 'Screenshot' };

function useParam(): [ReturnType<typeof parseExplorerParams>, (changes: Partial<Record<FilterParam, string | null>>) => void] {
  const [sp, update] = useLatestSearchParams();
  return [parseExplorerParams(sp), update];
}

function Count({ n }: { n: number | undefined }) {
  return (
    <Text span size="xs" c="dimmed" className="fp-f-count">
      {n === undefined ? '' : n.toLocaleString()}
    </Text>
  );
}

function Check({ label, count, checked, onChange, testId }: { label: ReactNode; count?: number; checked: boolean; onChange: (on: boolean) => void; testId: string }) {
  return (
    <Checkbox
      size="xs"
      checked={checked}
      onChange={(e) => onChange(e.currentTarget.checked)}
      data-testid={testId}
      classNames={{ body: 'fp-f-check', labelWrapper: 'fp-f-label' }}
      label={
        <Group gap={6} wrap="nowrap" justify="space-between" style={{ width: '100%' }}>
          <Text span size="xs" className="fh-truncate">
            {label}
          </Text>
          <Count n={count} />
        </Group>
      }
    />
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <Stack gap={4}>
      <Text size="xs" fw={700} c="dimmed" tt="uppercase">
        {title}
      </Text>
      {children}
    </Stack>
  );
}

function secondsToMs(v: string | number): string | null {
  if (v === '') return null;
  const n = Number(v);
  return Number.isFinite(n) && n >= 0 ? String(Math.round(n * 1000)) : null;
}

function FilterBody({ facets, defectTypes }: { facets: LaunchFacets | undefined; defectTypes: Map<string, DefectType> }) {
  const [p, set] = useParam();
  const urlMessage = p.message ?? '';
  const [message, setMessage] = useState(urlMessage);
  const [seen, setSeen] = useState(urlMessage);
  if (urlMessage !== seen) {
    setSeen(urlMessage);
    if (urlMessage !== message.trim()) setMessage(urlMessage);
  }
  const [debounced] = useDebouncedValue(message, 300);
  const pushed = useRef(debounced);
  useEffect(() => {
    if (pushed.current === debounced) return;
    pushed.current = debounced;
    set({ message: debounced.trim() || null });
  }, [debounced, set]);
  const f = facets;
  const list = (k: FilterParam, value: string, on: boolean) => set({ [k]: toggleValue(p[k as keyof typeof p] as string | undefined, value, on) });
  const has = (k: 'severity' | 'owner' | 'tag' | 'defect' | 'attachment', value: string) => listValues(p[k]).includes(value);
  return (
    <Stack gap="sm" data-testid="explorer-filters-menu">
      <Section title="Marks">
        <Check label="Retried" count={f?.retried} checked={!!p.retried} onChange={(on) => set({ retried: on ? 'true' : null })} testId="filter-retried" />
        <Check label="New failures" count={f?.newFailures} checked={!!p.newFailures} onChange={(on) => set({ newFailures: on ? 'true' : null })} testId="filter-new-failures" />
        <Check label="Muted" count={f?.counts.muted} checked={p.muted === true} onChange={(on) => set({ muted: on ? 'true' : null })} testId="filter-muted" />
        {f && f.manual > 0 && (
          <>
            <Check label="Manual" count={f.manual} checked={p.source === 'manual'} onChange={(on) => set({ source: on ? 'manual' : null })} testId="filter-manual" />
            <Check label="Automated" count={f.automated} checked={p.source === 'automated'} onChange={(on) => set({ source: on ? 'automated' : null })} testId="filter-automated" />
          </>
        )}
      </Section>
      {f?.labels.map((l) =>
        l.values.length ? (
          <Section key={l.name} title={LABELS[l.name] ?? l.name}>
            {l.values.map((v) => (
              <Check key={v.value} label={v.value} count={v.count} checked={has(l.name as 'severity', v.value)} onChange={(on) => list(l.name as FilterParam, v.value, on)} testId={`filter-${l.name}-${v.value}`} />
            ))}
            {l.none > 0 && <Check label={`No ${(LABELS[l.name] ?? l.name).toLowerCase()}`} count={l.none} checked={has(l.name as 'severity', NONE_VALUE)} onChange={(on) => list(l.name as FilterParam, NONE_VALUE, on)} testId={`filter-${l.name}-none`} />}
            {l.more && (
              <Text size="xs" c="dimmed">
                The most frequent values are listed.
              </Text>
            )}
          </Section>
        ) : null,
      )}
      {f && f.defects.length > 0 && (
        <Section title="Defect type">
          {f.defects.map((d) => (
            <Check key={d.value} label={d.value === 'none' ? 'No defect' : (defectTypes.get(d.value)?.name ?? 'Unknown type')} count={d.count} checked={has('defect', d.value)} onChange={(on) => list('defect', d.value, on)} testId={`filter-defect-${d.value}`} />
          ))}
        </Section>
      )}
      <Section title="Attachments">
        {(f?.attachments ?? Object.keys(ATTACHMENTS).map((value) => ({ value, count: 0 }))).map((a) => (
          <Check key={a.value} label={ATTACHMENTS[a.value] ?? a.value} count={f ? a.count : undefined} checked={has('attachment', a.value)} onChange={(on) => list('attachment', a.value, on)} testId={`filter-attachment-${a.value}`} />
        ))}
      </Section>
      <Section title="Duration, seconds">
        <Group gap="xs" wrap="nowrap" grow>
          <NumberInput size="xs" aria-label="Shortest duration in seconds" placeholder="from" min={0} decimalScale={3} value={p.minDurationMs === undefined ? '' : p.minDurationMs / 1000} onChange={(v) => set({ minDurationMs: secondsToMs(v) })} data-testid="filter-min-duration" />
          <NumberInput size="xs" aria-label="Longest duration in seconds" placeholder={f?.maxDurationMs ? `to ${String(Math.ceil(f.maxDurationMs / 1000))}` : 'to'} min={0} decimalScale={3} value={p.maxDurationMs === undefined ? '' : p.maxDurationMs / 1000} onChange={(v) => set({ maxDurationMs: secondsToMs(v) })} data-testid="filter-max-duration" />
        </Group>
      </Section>
      <Section title="Error message">
        <TextInput size="xs" aria-label="Text in the error message" placeholder="Text in the error message" value={message} onChange={(e) => setMessage(e.currentTarget.value)} data-testid="filter-message" />
      </Section>
    </Stack>
  );
}

/** The filter menu of the explorer and the result list: the values with their counts. */
export function FilterMenu({ project, launch }: { project: string; launch: Launch }) {
  const [p] = useParam();
  const n = extraCount(p);
  const [opened, setOpened] = useState(false);
  const facets = useLaunchFacets(project, launch.id, p.env, launch.status === 'running', opened || n > 0, opened);
  const defectTypes = useDefectTypeMap(project);
  return (
    <Popover opened={opened} onChange={setOpened} position="bottom-start" width="min(340px, calc(100vw - 32px))" shadow="md" withinPortal trapFocus returnFocus>
      <Popover.Target>
        <Button
          size="compact-xs"
          variant={n ? 'light' : 'default'}
          leftSection={<IconFilter size={13} />}
          rightSection={n ? <Badge size="xs" circle>{n}</Badge> : undefined}
          onClick={() => setOpened((o) => !o)}
          aria-expanded={opened}
          aria-haspopup="dialog"
          data-testid="explorer-filters"
        >
          Filters
        </Button>
      </Popover.Target>
      <Popover.Dropdown p="xs" aria-label="Filters">
        <ScrollArea.Autosize mah="min(70vh, 560px)" type="auto" offsetScrollbars>
          <FilterBody facets={facets.data} defectTypes={defectTypes} />
        </ScrollArea.Autosize>
      </Popover.Dropdown>
    </Popover>
  );
}

interface Pill {
  key: string;
  label: string;
  clear: Partial<Record<FilterParam, null>>;
}

function seconds(ms: number): string {
  return `${String(Math.round(ms / 100) / 10)} s`;
}

/** The filters that are set, each removable, with a way to clear them all. */
export function ActiveFilters({ project }: { project: string }) {
  const [p, set] = useParam();
  const defectTypes = useDefectTypeMap(project);
  const pills = (() => {
    const out: Pill[] = [];
    const names = (v: string | undefined, f: (x: string) => string = (x) => x) => listValues(v).map(f).join(', ');
    if (p.retried) out.push({ key: 'retried', label: 'Retried', clear: { retried: null } });
    if (p.newFailures) out.push({ key: 'newFailures', label: 'New failures', clear: { newFailures: null } });
    if (p.muted !== undefined) out.push({ key: 'muted', label: p.muted ? 'Muted' : 'Not muted', clear: { muted: null } });
    if (p.source) out.push({ key: 'source', label: p.source === 'manual' ? 'Manual' : 'Automated', clear: { source: null } });
    for (const k of ['severity', 'owner', 'tag'] as const) {
      if (p[k]) out.push({ key: k, label: `${LABELS[k] ?? k}: ${names(p[k], (x) => (x === NONE_VALUE ? 'none' : x))}`, clear: { [k]: null } });
    }
    if (p.defect) out.push({ key: 'defect', label: `Defect: ${names(p.defect, (x) => (x === 'none' ? 'none' : (defectTypes.get(x)?.name ?? 'unknown')))}`, clear: { defect: null } });
    if (p.attachment) out.push({ key: 'attachment', label: `Has ${names(p.attachment, (x) => (ATTACHMENTS[x] ?? x).toLowerCase())}`, clear: { attachment: null } });
    if (p.minDurationMs !== undefined || p.maxDurationMs !== undefined) {
      const label = p.minDurationMs !== undefined && p.maxDurationMs !== undefined ? `${seconds(p.minDurationMs)} – ${seconds(p.maxDurationMs)}` : p.minDurationMs !== undefined ? `≥ ${seconds(p.minDurationMs)}` : `≤ ${seconds(p.maxDurationMs ?? 0)}`;
      out.push({ key: 'duration', label: `Duration ${label}`, clear: { minDurationMs: null, maxDurationMs: null } });
    }
    if (p.message) out.push({ key: 'message', label: `Message: “${p.message}”`, clear: { message: null } });
    return out;
  })();
  if (!pills.length) return null;
  const clearAll: Partial<Record<FilterParam, null>> = Object.assign({}, ...pills.map((x) => x.clear)) as Partial<Record<FilterParam, null>>;
  return (
    <Group gap={4} wrap="wrap" data-testid="explorer-active-filters" aria-label="Active filters" role="group">
      {pills.map((x) => (
        <Badge
          key={x.key}
          variant="light"
          color="fhPurple"
          radius="sm"
          style={{ textTransform: 'none', maxWidth: '100%' }}
          rightSection={<CloseButton size="xs" aria-label={`Remove ${x.label}`} onClick={() => set(x.clear)} icon={<IconX size={10} />} />}
          data-testid={`active-filter-${x.key}`}
        >
          <span className="fh-truncate">{x.label}</span>
        </Badge>
      ))}
      <Button variant="subtle" color="gray" size="compact-xs" onClick={() => set(clearAll)} data-testid="explorer-clear-filters">
        Clear
      </Button>
    </Group>
  );
}
