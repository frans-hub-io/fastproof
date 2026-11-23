import type { Step } from '../../api/types';

export const FRAME_SECONDS = 0.04;
export const SPEEDS: readonly number[] = [0.25, 0.5, 0.75, 1, 1.25, 1.5, 2];
export const OFFSET_STEP_MS = 500;
export const MAX_OFFSET_MS = 30_000;

export function clamp(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(max, Math.max(min, value));
}

/** `m:ss`, or `h:mm:ss` from an hour on; `--:--` for values that are not finite. */
export function formatClock(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined || !Number.isFinite(seconds) || seconds < 0) return '--:--';
  const total = Math.floor(seconds);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, '0');
  return h > 0 ? `${String(h)}:${String(m).padStart(2, '0')}:${ss}` : `${String(m)}:${ss}`;
}

/** A spoken form of the clock, for the live region. */
export function spokenClock(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const m = Math.floor(total / 60);
  const s = total % 60;
  if (m === 0) return `${String(s)} seconds`;
  return `${String(m)} minutes ${String(s)} seconds`;
}

export interface TimeRangesLike {
  length: number;
  start(i: number): number;
  end(i: number): number;
}

export interface Range {
  start: number;
  end: number;
}

export function rangesOf(r: TimeRangesLike | null | undefined): Range[] {
  if (!r) return [];
  const out: Range[] = [];
  for (let i = 0; i < r.length; i++) {
    const start = r.start(i);
    const end = r.end(i);
    if (Number.isFinite(start) && Number.isFinite(end) && end > start) out.push({ start, end });
  }
  return out;
}

export interface MediaLike {
  duration: number;
  seekable: TimeRangesLike;
  buffered: TimeRangesLike;
  currentTime: number;
}

export interface Bounds {
  /** Seconds the seek bar spans; `null` when the length is not known yet. */
  length: number | null;
  /** Latest time a seek can reach. */
  max: number;
  known: boolean;
}

/**
 * WebM written while recording has no duration (`Infinity`, or `NaN` before the metadata). Then the
 * bar spans what the browser can seek to, or has loaded, and only elapsed time is shown.
 */
export function boundsOf(m: MediaLike): Bounds {
  if (Number.isFinite(m.duration) && m.duration > 0) return { length: m.duration, max: m.duration, known: true };
  const ends = [...rangesOf(m.seekable), ...rangesOf(m.buffered)].map((r) => r.end);
  const max = Math.max(m.currentTime, ...ends, 0);
  return { length: null, max, known: false };
}

export function fractionOf(time: number, bounds: Bounds): number {
  if (bounds.max <= 0) return 0;
  return clamp(time / bounds.max, 0, 1);
}

export function timeAtFraction(fraction: number, bounds: Bounds): number {
  return clamp(fraction, 0, 1) * bounds.max;
}

export function seekTarget(current: number, delta: number, bounds: Bounds): number {
  return clamp(current + delta, 0, bounds.max);
}

export type PlayerAction =
  | { type: 'toggle' }
  | { type: 'seekBy'; seconds: number }
  | { type: 'frame'; direction: 1 | -1 }
  | { type: 'seekStart' }
  | { type: 'seekEnd' }
  | { type: 'fullscreen' }
  | { type: 'mute' }
  | { type: 'speed'; direction: 1 | -1 };

export interface KeyLike {
  key: string;
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
}

/** The keyboard map. Frame stepping only applies while paused. */
export function keyAction(e: KeyLike, paused: boolean): PlayerAction | null {
  if (e.ctrlKey || e.metaKey || e.altKey) return null;
  switch (e.key) {
    case ' ':
    case 'k':
    case 'K':
      return { type: 'toggle' };
    case 'ArrowLeft':
      return { type: 'seekBy', seconds: e.shiftKey ? -60 : -5 };
    case 'ArrowRight':
      return { type: 'seekBy', seconds: e.shiftKey ? 60 : 5 };
    case ',':
      return paused ? { type: 'frame', direction: -1 } : null;
    case '.':
      return paused ? { type: 'frame', direction: 1 } : null;
    case 'Home':
      return { type: 'seekStart' };
    case 'End':
      return { type: 'seekEnd' };
    case 'f':
    case 'F':
      return { type: 'fullscreen' };
    case 'm':
    case 'M':
      return { type: 'mute' };
    case '<':
      return { type: 'speed', direction: -1 };
    case '>':
      return { type: 'speed', direction: 1 };
    default:
      return null;
  }
}

export function nextSpeed(current: number, direction: 1 | -1): number {
  const i = SPEEDS.findIndex((s) => s >= current);
  const at = i === -1 ? SPEEDS.length - 1 : i;
  const next = clamp(at + direction, 0, SPEEDS.length - 1);
  return SPEEDS[next] ?? 1;
}

export function validSpeed(v: unknown): number {
  return typeof v === 'number' && SPEEDS.includes(v) ? v : 1;
}

export function validVolume(v: unknown): number {
  return typeof v === 'number' && Number.isFinite(v) ? clamp(v, 0, 1) : 1;
}

export interface Prefs {
  speed: number;
  volume: number;
  muted: boolean;
}

export const PREFS_KEY = 'fastproof.player';

export function loadPrefs(storage: Pick<Storage, 'getItem'> | null): Prefs {
  const fallback: Prefs = { speed: 1, volume: 1, muted: false };
  try {
    const raw = storage?.getItem(PREFS_KEY);
    if (!raw) return fallback;
    const p = JSON.parse(raw) as Partial<Prefs>;
    return { speed: validSpeed(p.speed), volume: validVolume(p.volume), muted: p.muted === true };
  } catch {
    return fallback;
  }
}

export function savePrefs(storage: Pick<Storage, 'setItem'> | null, prefs: Prefs): void {
  try {
    storage?.setItem(PREFS_KEY, JSON.stringify(prefs));
  } catch {
    /* storage unavailable */
  }
}

export function browserStorage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export interface Marker {
  key: string;
  name: string;
  status: string;
  depth: number;
  /** Seconds into the video, offset included. */
  time: number;
  /** Seconds into the video where the step ended, when known. */
  end: number | null;
  failed: boolean;
  kind: string;
}

const FAILED = new Set(['failed', 'broken']);

function ms(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const t = new Date(iso).getTime();
  return Number.isFinite(t) ? t : null;
}

export interface StepRow {
  key: string;
  depth: number;
  step: Step;
}

/** Steps in document order, each with a key that is stable for a given tree. */
export function flattenSteps(steps: readonly Step[]): StepRow[] {
  const out: StepRow[] = [];
  const walk = (list: readonly Step[], depth: number, path: string) => {
    list.forEach((step, i) => {
      const key = `${path}${String(i)}`;
      out.push({ key, depth, step });
      walk(step.steps, depth + 1, `${key}.`);
    });
  };
  walk(steps, 0, '');
  return out;
}

/**
 * Markers for the steps with their time in the video: `step.startedAt - attempt.startedAt` plus the
 * nudge. A step without a start gets no marker (its children still do).
 */
export function buildMarkers(attemptStartedAt: string | null | undefined, steps: readonly Step[], offsetMs: number): Marker[] {
  const base = ms(attemptStartedAt);
  if (base === null) return [];
  const out: Marker[] = [];
  for (const { key, depth, step: s } of flattenSteps(steps)) {
    const start = ms(s.startedAt);
    if (start === null) continue;
    const stop = ms(s.stoppedAt) ?? (s.durationMs !== null && s.durationMs !== undefined ? start + s.durationMs : null);
    out.push({
      key,
      name: s.name,
      status: s.status,
      depth,
      time: Math.max(0, (start - base + offsetMs) / 1000),
      end: stop === null ? null : Math.max(0, (stop - base + offsetMs) / 1000),
      failed: FAILED.has(s.status),
      kind: s.kind,
    });
  }
  return out;
}

/** The moment of the failure: the end of the deepest failed step that has a time. */
export function failureTime(markers: readonly Marker[]): number | null {
  let best: Marker | null = null;
  for (const m of markers) {
    if (!m.failed) continue;
    if (!best || m.depth >= best.depth) best = m;
  }
  if (!best) return null;
  return best.end ?? best.time;
}

/** The innermost step running at `time`; the last one started when none is still running. */
export function currentMarker(markers: readonly Marker[], time: number): Marker | null {
  let started: Marker | null = null;
  let running: Marker | null = null;
  for (const m of markers) {
    if (m.time > time) continue;
    started = m;
    if (m.end === null || m.end > time) running = m;
  }
  return running ?? started;
}

export function clampOffset(ms: number): number {
  return clamp(Math.round(ms), -MAX_OFFSET_MS, MAX_OFFSET_MS);
}

/** `id:ms,id:ms` as kept in the address; entries that do not parse are dropped. */
export function parseOffsets(param: string | null | undefined): Record<string, number> {
  const out: Record<string, number> = {};
  if (!param) return out;
  for (const part of param.split(',').slice(0, 16)) {
    const at = part.lastIndexOf(':');
    if (at <= 0) continue;
    const id = part.slice(0, at);
    const value = Number(part.slice(at + 1));
    if (/^[0-9a-zA-Z-]{1,64}$/.test(id) && Number.isFinite(value) && value !== 0) out[id] = clampOffset(value);
  }
  return out;
}

export function serializeOffsets(offsets: Record<string, number>): string | null {
  const parts = Object.entries(offsets)
    .filter(([, v]) => v !== 0)
    .map(([id, v]) => `${id}:${String(clampOffset(v))}`);
  return parts.length ? parts.join(',') : null;
}

export function formatOffset(ms: number): string {
  if (ms === 0) return '0.0 s';
  return `${ms > 0 ? '+' : '-'}${(Math.abs(ms) / 1000).toFixed(1)} s`;
}

export const REFRESH_MARGIN_MS = 60_000;
export const MIN_REFRESH_MS = 5_000;

/** Milliseconds until the media address should be replaced: a minute before it expires. */
export function refreshDelay(expiresAt: string | number, now: number): number {
  const at = typeof expiresAt === 'number' ? expiresAt : new Date(expiresAt).getTime();
  if (!Number.isFinite(at)) return 10 * 60_000;
  return Math.max(MIN_REFRESH_MS, at - now - REFRESH_MARGIN_MS);
}

export type PlaybackProblem = 'expired' | 'unsupported' | 'network';

/** Tell an address that stopped working from a file the browser cannot decode. */
export function classifyError(code: number | undefined, probeStatus: number | null): PlaybackProblem {
  if (probeStatus === 404 || probeStatus === 410 || probeStatus === 403) return 'expired';
  if (code === 3 || code === 4) return 'unsupported';
  return 'network';
}

/** Whether a refresh is still allowed: at most `max` replacements within `windowMs`. */
export function refreshAllowed(history: readonly number[], now: number, max = 3, windowMs = 15_000): boolean {
  return history.filter((t) => now - t < windowMs).length < max;
}

/** The second a hover thumbnail belongs to; the cache is keyed by it. */
export function thumbSecond(time: number, bounds: Bounds): number {
  return Math.floor(clamp(time, 0, bounds.max));
}
