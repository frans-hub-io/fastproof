import { useEffect, useRef, useState } from 'react';
import { projectPath, request, withQuery } from './client';
import { eventStreamSupported, openEventStream } from './sse';
import type { Page } from './types';
import type { LogFilters, LogLevel, LogLine } from './types-next';
import { LOG_LEVELS } from './types-next';

export type LogEntry = { kind: 'line'; key: string; line: LogLine } | { kind: 'gap'; key: string; skipped: number };

export const LOG_CAP = 250_000;

function asNumber(v: unknown): number | null {
  return typeof v === 'number' && Number.isFinite(v) ? v : null;
}

function normalizeLevel(v: unknown): LogLevel {
  const s = typeof v === 'string' ? v.toLowerCase() : '';
  if (s === 'warning') return 'warn';
  if (s === 'fatal' || s === 'critical') return 'error';
  return (LOG_LEVELS as readonly string[]).includes(s) ? (s as LogLevel) : 'info';
}

/** `ts` of a line: milliseconds since the epoch (or an RFC 3339 string) as ISO 8601. */
function isoTime(v: unknown): string {
  const ms = typeof v === 'number' ? v : typeof v === 'string' ? Date.parse(v) : NaN;
  return Number.isFinite(ms) ? new Date(ms).toISOString() : new Date(0).toISOString();
}

export function toLogLine(raw: unknown): LogLine | null {
  if (!raw || typeof raw !== 'object') return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.message !== 'string') return null;
  return {
    resultId: typeof r.resultId === 'string' ? r.resultId : '',
    attempt: asNumber(r.attempt) ?? 0,
    seq: asNumber(r.seq) ?? 0,
    ts: isoTime(r.ts),
    level: normalizeLevel(r.level),
    stream: r.stream === 'stderr' || r.stream === 'log' || r.stream === 'step' ? r.stream : 'stdout',
    message: r.message,
    step: typeof r.step === 'string' ? r.step : null,
  };
}

/** Lines of a `log` SSE event: an array, `{lines}`, `{items}` or a single line. */
export function parseLogEvent(data: string): LogLine[] {
  let body: unknown;
  try {
    body = JSON.parse(data);
  } catch {
    return [];
  }
  const list = Array.isArray(body) ? body : body && typeof body === 'object' ? ((body as { lines?: unknown }).lines ?? (body as { items?: unknown }).items ?? [body]) : [];
  return Array.isArray(list) ? list.map(toLogLine).filter((l): l is LogLine => l !== null) : [];
}

/** Lines dropped by the server for a slow client, from a `gap` SSE event. */
export function parseGapEvent(data: string): number {
  try {
    const b = JSON.parse(data) as Record<string, unknown>;
    return asNumber(b.skipped) ?? asNumber(b.dropped) ?? asNumber(b.count) ?? asNumber(b.lines) ?? 0;
  } catch {
    return 0;
  }
}

export function lineKey(l: LogLine): string {
  return `${l.resultId}:${String(l.attempt)}:${String(l.seq)}`;
}

export function compareLines(a: LogLine, b: LogLine): number {
  if (a.ts !== b.ts) return a.ts < b.ts ? -1 : 1;
  if (a.resultId !== b.resultId) return a.resultId < b.resultId ? -1 : 1;
  if (a.attempt !== b.attempt) return a.attempt - b.attempt;
  return a.seq - b.seq;
}

function query(f: LogFilters) {
  return { resultId: f.resultId, attempt: f.attempt, level: f.level, q: f.q, env: f.env };
}

export function logsUrl(key: string, launchId: string): string {
  return `${projectPath(key)}/launches/${encodeURIComponent(launchId)}/logs`;
}

export function fetchLogs(key: string, launchId: string, f: LogFilters, before: string | undefined, limit: number): Promise<Page<LogLine>> {
  return request<Page<unknown>>(withQuery(logsUrl(key, launchId), { ...query(f), before, limit })).then((p) => ({
    items: p.items.map(toLogLine).filter((l): l is LogLine => l !== null),
    next: p.next ?? null,
  }));
}

export type StreamState = 'idle' | 'connecting' | 'live' | 'reconnecting' | 'closed';

export interface LogBuffer {
  entries: LogEntry[];
  /** Lines dropped from the front of the buffer to keep it under `LOG_CAP`. */
  trimmed: number;
}

export function appendEntries(buf: LogBuffer, add: LogEntry[], cap = LOG_CAP): LogBuffer {
  if (!add.length) return buf;
  const entries = buf.entries.concat(add);
  const over = entries.length - cap;
  if (over <= 0) return { entries, trimmed: buf.trimmed };
  return { entries: entries.slice(over), trimmed: buf.trimmed + over };
}

/**
 * Live lines of a launch over SSE (`GET .../logs/stream`). Lines are buffered and flushed at most
 * every 200 ms so a fast producer does not re-render on every event.
 */
export function useLogStream(key: string, launchId: string, f: LogFilters, enabled: boolean): { buffer: LogBuffer; state: StreamState; clear: () => void } {
  const [buffer, setBuffer] = useState<LogBuffer>({ entries: [], trimmed: 0 });
  const [state, setState] = useState<StreamState>('idle');
  const pending = useRef<LogEntry[]>([]);
  const gapSeq = useRef(0);
  const { resultId, attempt, level, q } = f;

  useEffect(() => {
    setBuffer({ entries: [], trimmed: 0 });
    pending.current = [];
    if (!enabled || !eventStreamSupported()) {
      setState(enabled ? 'closed' : 'idle');
      return;
    }
    setState('connecting');
    const timer = setInterval(() => {
      if (!pending.current.length) return;
      const add = pending.current;
      pending.current = [];
      setBuffer((b) => appendEntries(b, add));
    }, 200);
    const es = openEventStream(withQuery(`${logsUrl(key, launchId)}/stream`, query({ resultId, attempt, level, q })), {
      onOpen: () => setState('live'),
      onError: (closed) => setState(closed ? 'closed' : 'reconnecting'),
      events: {
        log: (data) => {
          for (const line of parseLogEvent(data)) pending.current.push({ kind: 'line', key: lineKey(line), line });
        },
        gap: (data) => {
          const skipped = parseGapEvent(data);
          if (skipped > 0) pending.current.push({ kind: 'gap', key: `gap:${String(++gapSeq.current)}`, skipped });
        },
        end: () => {
          es.close();
          setState('closed');
        },
      },
    });
    return () => {
      es.close();
      clearInterval(timer);
    };
  }, [key, launchId, resultId, attempt, level, q, enabled]);

  return { buffer, state, clear: () => setBuffer({ entries: [], trimmed: 0 }) };
}
