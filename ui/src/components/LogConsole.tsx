import { ActionIcon, Badge, Button, Group, Loader, Select, Stack, Text, TextInput, Tooltip } from '@mantine/core';
import { useDebouncedValue } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconArrowDown, IconCopy, IconDownload, IconPlayerPause, IconSearch, IconTrash } from '@tabler/icons-react';
import { useInfiniteQuery } from '@tanstack/react-query';
import { memo, useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type WheelEvent } from 'react';
import { Virtuoso, type VirtuosoHandle } from 'react-virtuoso';
import { ApiError, errorMessage } from '../api/client';
import { appendEntries, fetchLogs, lineKey, LOG_CAP, useLogStream, type LogEntry } from '../api/logs';
import type { LogFilters, LogLevel } from '../api/types-next';
import { parseAnsi, stripAnsi, type AnsiStyle } from '../lib/ansi';

const PAGE = 5000;
const COPY_MAX = 100_000;
const FIRST_INDEX = 1_000_000_000;

const LEVEL_OPTIONS: { value: string; label: string }[] = [
  { value: '', label: 'All levels' },
  { value: 'debug', label: 'Debug and up' },
  { value: 'info', label: 'Info and up' },
  { value: 'warn', label: 'Warnings and errors' },
  { value: 'error', label: 'Errors only' },
];

function time(ts: string): string {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return ts.slice(11, 23);
  const p = (n: number, w = 2) => String(n).padStart(w, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}

function css(s: AnsiStyle) {
  if (!s.fg && !s.bg && !s.bold && !s.dim && !s.italic && !s.underline) return undefined;
  return {
    color: s.fg,
    background: s.bg,
    fontWeight: s.bold ? 700 : undefined,
    opacity: s.dim ? 0.7 : undefined,
    fontStyle: s.italic ? 'italic' : undefined,
    textDecoration: s.underline ? 'underline' : undefined,
  };
}

const Message = memo(function Message({ text }: { text: string }) {
  const segments = useMemo(() => parseAnsi(text), [text]);
  return (
    <span className="fp-console-msg">
      {segments.map((s, i) => {
        const style = css(s.style);
        return style ? (
          <span key={i} style={style}>
            {s.text}
          </span>
        ) : (
          s.text
        );
      })}
    </span>
  );
});

function Row({ entry, names, showResult }: { entry: LogEntry; names: Map<string, string>; showResult: boolean }) {
  if (entry.kind === 'gap') {
    return (
      <div className="fp-console-gap" data-testid="log-gap">
        ⋯ {entry.skipped.toLocaleString()} line{entry.skipped === 1 ? '' : 's'} skipped: the console fell behind the stream
      </div>
    );
  }
  const l = entry.line;
  return (
    <div className="fp-console-row" data-level={l.level} data-testid="log-line">
      <span className="fp-console-time">{time(l.ts)}</span>
      <span className="fp-console-level">{l.level}</span>
      {showResult && (
        <span className="fp-console-result" title={names.get(l.resultId) ?? l.resultId}>
          {names.get(l.resultId) ?? l.resultId.slice(0, 8)}
          {l.attempt > 1 ? ` #${String(l.attempt)}` : ''}
        </span>
      )}
      <Message text={l.message} />
    </div>
  );
}

function plain(entries: LogEntry[], names: Map<string, string>, showResult: boolean): string {
  return entries
    .map((e) =>
      e.kind === 'gap'
        ? `... ${String(e.skipped)} lines skipped ...`
        : `${e.line.ts} ${e.line.level.toUpperCase().padEnd(5)} ${showResult ? `[${names.get(e.line.resultId) ?? e.line.resultId}] ` : ''}${stripAnsi(e.line.message)}`,
    )
    .join('\n');
}

export interface LogConsoleProps {
  project: string;
  launchId: string;
  /** Stream new lines over SSE (running launch); otherwise read the stored logs. */
  live: boolean;
  /** Fixed result (result page); without it the console shows the whole launch with a result filter. */
  resultId?: string;
  attempt?: number;
  /** Result names for the result column and filter. */
  results?: { id: string; name: string }[];
  fileName?: string;
  height?: string;
}

export function LogConsole({ project, launchId, live, resultId, attempt, results = [], fileName = 'logs.txt', height = 'min(62vh, 640px)' }: LogConsoleProps) {
  const [pickedResult, setPickedResult] = useState<string | null>(null);
  const [level, setLevel] = useState('');
  const [text, setText] = useState('');
  const [q] = useDebouncedValue(text.trim(), 350);
  const filters: LogFilters = useMemo(
    () => ({ resultId: resultId ?? pickedResult ?? undefined, attempt, level: (level || undefined) as LogLevel | undefined, q: q || undefined }),
    [resultId, pickedResult, attempt, level, q],
  );
  const names = useMemo(() => new Map(results.map((r) => [r.id, r.name] as const)), [results]);
  const showResult = !resultId;

  const stored = useInfiniteQuery({
    queryKey: ['logs', project, launchId, filters],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => fetchLogs(project, launchId, filters, pageParam, PAGE),
    getNextPageParam: (last) => last.next ?? undefined,
    staleTime: live ? 0 : 60_000,
    retry: false,
  });
  const unsupported = stored.error instanceof ApiError && stored.error.status === 404 && stored.error.message === 'resource not found';
  const stream = useLogStream(project, launchId, filters, live && !unsupported);

  const entries = useMemo(() => {
    const pages = stored.data?.pages ?? [];
    const old: LogEntry[] = [];
    const seen = new Set<string>();
    for (let i = pages.length - 1; i >= 0; i--) {
      for (const line of pages[i]?.items ?? []) {
        const key = lineKey(line);
        if (seen.has(key)) continue;
        seen.add(key);
        old.push({ kind: 'line', key, line });
      }
    }
    if (!live) return { list: old, trimmed: 0, older: old.length };
    const fresh = stream.buffer.entries.filter((e) => e.kind === 'gap' || !seen.has(e.key));
    const merged = appendEntries({ entries: old, trimmed: 0 }, fresh, LOG_CAP);
    return { list: merged.entries, trimmed: merged.trimmed + stream.buffer.trimmed, older: old.length };
  }, [stored.data, stream.buffer, live]);

  const list = entries.list;
  const lineCount = useMemo(() => list.reduce((n, e) => n + (e.kind === 'line' ? 1 : 0), 0), [list]);
  const skipped = useMemo(() => list.reduce((n, e) => n + (e.kind === 'gap' ? e.skipped : 0), 0), [list]);
  const [loadAll, setLoadAll] = useState(false);
  const { hasNextPage, isFetchingNextPage, fetchNextPage } = stored;
  useEffect(() => {
    if (!loadAll) return;
    if (!hasNextPage || entries.older >= LOG_CAP) {
      setLoadAll(false);
      return;
    }
    if (!isFetchingNextPage) void fetchNextPage();
  }, [loadAll, hasNextPage, isFetchingNextPage, fetchNextPage, entries.older]);
  useEffect(() => {
    setLoadAll(false);
  }, [filters]);

  const ref = useRef<VirtuosoHandle>(null);
  const [paused, setPaused] = useState(false);
  const [pausedAt, setPausedAt] = useState(0);
  const interacted = useRef(0);
  const following = useRef(false);
  const length = useRef(0);
  useEffect(() => {
    length.current = list.length;
  }, [list.length]);
  const unseen = paused ? Math.max(0, list.length - pausedAt) : 0;

  const markInteraction = useCallback(() => {
    interacted.current = Date.now();
  }, []);

  const pauseNow = useCallback(() => {
    interacted.current = Date.now();
    setPaused((p) => {
      if (!p) setPausedAt(length.current);
      return true;
    });
  }, []);

  const onWheel = useCallback(
    (e: WheelEvent) => {
      if (e.deltaY < 0) pauseNow();
      else markInteraction();
    },
    [pauseNow, markInteraction],
  );

  const onKey = useCallback(
    (e: KeyboardEvent) => {
      if (e.key === 'PageUp' || e.key === 'ArrowUp' || e.key === 'Home') pauseNow();
      else markInteraction();
    },
    [pauseNow, markInteraction],
  );

  const onBottom = useCallback((bottom: boolean) => {
    if (bottom) {
      following.current = false;
      setPaused(false);
    } else if (!following.current && Date.now() - interacted.current < 1500) {
      setPausedAt(length.current);
      setPaused(true);
    }
  }, []);

  useEffect(() => {
    setPaused(false);
  }, [filters]);

  const resume = () => {
    interacted.current = 0;
    following.current = true;
    setPaused(false);
    ref.current?.scrollToIndex({ index: 'LAST', align: 'end', behavior: 'auto' });
  };

  const copy = () => {
    const slice = list.length > COPY_MAX ? list.slice(-COPY_MAX) : list;
    navigator.clipboard
      .writeText(plain(slice, names, showResult))
      .then(() =>
        notifications.show({ color: 'green', message: list.length > COPY_MAX ? `Copied the last ${COPY_MAX.toLocaleString()} lines; use Download for all` : `Copied ${slice.length.toLocaleString()} lines` }),
      )
      .catch((e: unknown) => notifications.show({ color: 'red', title: 'Copy failed', message: errorMessage(e) }));
  };

  const download = () => {
    const blob = new Blob([plain(list, names, showResult), '\n'], { type: 'text/plain;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = fileName.replace(/[^A-Za-z0-9._-]/g, '_');
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
  };

  const streamLabel = stream.state === 'live' ? 'Live' : stream.state === 'connecting' ? 'Connecting' : stream.state === 'reconnecting' ? 'Reconnecting' : 'Stream ended';
  const streamColor = stream.state === 'live' ? 'green' : stream.state === 'closed' ? 'gray' : 'yellow';
  const loadingFirst = stored.isPending && !live;

  return (
    <Stack gap="xs" data-testid="log-console">
      <Group gap="xs" wrap="wrap" align="flex-end">
        {!resultId && results.length > 0 && (
          <Select
            aria-label="Result"
            placeholder="All results"
            data={results.map((r) => ({ value: r.id, label: r.name }))}
            value={pickedResult}
            onChange={setPickedResult}
            searchable
            clearable
            size="xs"
            w={{ base: '100%', sm: 240 }}
            comboboxProps={{ withinPortal: true }}
            data-testid="log-result-filter"
          />
        )}
        <Select aria-label="Level" data={LEVEL_OPTIONS} value={level} onChange={(v) => setLevel(v ?? '')} allowDeselect={false} size="xs" w={{ base: '48%', sm: 190 }} data-testid="log-level-filter" comboboxProps={{ withinPortal: true }} />
        <TextInput
          aria-label="Filter lines"
          placeholder="Filter text"
          leftSection={<IconSearch size={14} />}
          value={text}
          onChange={(e) => setText(e.currentTarget.value)}
          maxLength={200}
          size="xs"
          style={{ flex: '1 1 160px' }}
          data-testid="log-text-filter"
        />
        <Group gap={4} wrap="nowrap">
          <Tooltip label="Copy">
            <ActionIcon variant="default" onClick={copy} aria-label="Copy lines" disabled={!list.length}>
              <IconCopy size={15} />
            </ActionIcon>
          </Tooltip>
          <Tooltip label="Download">
            <ActionIcon variant="default" onClick={download} aria-label="Download lines" disabled={!list.length} data-testid="log-download">
              <IconDownload size={15} />
            </ActionIcon>
          </Tooltip>
          {live && (
            <Tooltip label="Clear">
              <ActionIcon variant="default" onClick={stream.clear} aria-label="Clear the console">
                <IconTrash size={15} />
              </ActionIcon>
            </Tooltip>
          )}
        </Group>
      </Group>
      <Group gap="sm" justify="space-between" wrap="wrap">
        <Group gap="xs">
          {live && (
            <Badge variant="dot" color={streamColor} size="sm" style={{ textTransform: 'none' }} data-testid="log-stream-state" data-state={stream.state}>
              {streamLabel}
            </Badge>
          )}
          <Text size="xs" c="dimmed" data-testid="log-count">
            {lineCount.toLocaleString()} line{lineCount === 1 ? '' : 's'}
            {entries.trimmed ? ` · ${entries.trimmed.toLocaleString()} oldest dropped from view` : ''}
          </Text>
          {skipped > 0 && (
            <Tooltip label="The server dropped lines because this browser could not keep up; they are in the stored logs once the launch finishes">
              <Badge size="sm" variant="light" color="yellow" style={{ textTransform: 'none' }} data-testid="log-gaps">
                {skipped.toLocaleString()} lines skipped
              </Badge>
            </Tooltip>
          )}
        </Group>
        {paused && (
          <Badge variant="light" color="gray" size="sm" leftSection={<IconPlayerPause size={11} />} style={{ textTransform: 'none' }} data-testid="log-paused">
            Auto-scroll paused
          </Badge>
        )}
      </Group>
      {(stored.hasNextPage || loadAll) && (
        <Group gap="xs">
          <Button size="xs" variant="subtle" onClick={() => void stored.fetchNextPage()} loading={stored.isFetchingNextPage && !loadAll} disabled={loadAll} data-testid="log-load-earlier">
            Load earlier lines
          </Button>
          <Button size="xs" variant="subtle" onClick={() => setLoadAll((v) => !v)} leftSection={loadAll ? <Loader size={12} /> : undefined} data-testid="log-load-all">
            {loadAll ? 'Stop loading' : `Load all (up to ${LOG_CAP.toLocaleString()} lines)`}
          </Button>
        </Group>
      )}
      <div className="fp-console" style={{ height }} onWheelCapture={onWheel} onTouchMoveCapture={markInteraction} onPointerDownCapture={markInteraction} onKeyDownCapture={onKey}>
        {loadingFirst ? (
          <Group justify="center" p="xl">
            <Loader size="sm" />
          </Group>
        ) : unsupported ? (
          <Text size="sm" c="dimmed" p="md" data-testid="log-unsupported">
            This FastProof server does not store structured logs yet. Attempt output is in the Output tab of each result.
          </Text>
        ) : stored.isError && !live ? (
          <Text size="sm" c="var(--mantine-color-error)" p="md">
            {errorMessage(stored.error)}
          </Text>
        ) : list.length === 0 ? (
          <Text size="sm" c="dimmed" p="md" data-testid="log-empty">
            {live ? 'Waiting for log lines…' : 'No log lines match.'}
          </Text>
        ) : (
          <Virtuoso
            ref={ref}
            data={list}
            computeItemKey={(_, e) => e.key}
            firstItemIndex={FIRST_INDEX - entries.older}
            followOutput={(bottom: boolean) => ((bottom || following.current) && !paused ? 'auto' : false)}
            atBottomStateChange={onBottom}
            atBottomThreshold={24}
            initialTopMostItemIndex={{ index: 'LAST', align: 'end' }}
            increaseViewportBy={400}
            tabIndex={0}
            itemContent={(_, e) => <Row entry={e} names={names} showResult={showResult} />}
            style={{ height: '100%' }}
            data-testid="log-list"
          />
        )}
        {paused && (
          <Button size="xs" radius="xl" className="fp-console-resume" leftSection={<IconArrowDown size={14} />} onClick={resume} data-testid="log-resume">
            {unseen > 0 ? `${unseen.toLocaleString()} new · ` : ''}Jump to latest
          </Button>
        )}
      </div>
    </Stack>
  );
}

