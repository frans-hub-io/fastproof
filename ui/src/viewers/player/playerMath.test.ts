import { describe, expect, it } from 'vitest';
import type { Step } from '../../api/types';
import {
  FRAME_SECONDS,
  boundsOf,
  buildMarkers,
  classifyError,
  clampOffset,
  currentMarker,
  failureTime,
  flattenSteps,
  formatClock,
  formatOffset,
  fractionOf,
  keyAction,
  loadPrefs,
  nextSpeed,
  parseOffsets,
  PREFS_KEY,
  rangesOf,
  refreshAllowed,
  refreshDelay,
  savePrefs,
  seekTarget,
  serializeOffsets,
  thumbSecond,
  timeAtFraction,
  type KeyLike,
  type TimeRangesLike,
} from './playerMath';

const ranges = (...r: [number, number][]): TimeRangesLike => ({ length: r.length, start: (i) => r[i]?.[0] ?? 0, end: (i) => r[i]?.[1] ?? 0 });
const key = (k: string, extra: Partial<KeyLike> = {}): KeyLike => ({ key: k, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, ...extra });

function step(name: string, startOffsetMs: number | null, durationMs: number, status = 'passed', steps: Step[] = []): Step {
  const base = Date.parse('2026-11-22T10:00:00.000Z');
  return {
    name,
    status: status as Step['status'],
    kind: 'step',
    startedAt: startOffsetMs === null ? null : new Date(base + startOffsetMs).toISOString(),
    stoppedAt: startOffsetMs === null ? null : new Date(base + startOffsetMs + durationMs).toISOString(),
    durationMs,
    error: null,
    parameters: [],
    attachments: [],
    steps,
  };
}

const START = '2026-11-22T10:00:00.000Z';

describe('formatClock', () => {
  it('formats minutes and hours and tolerates bad values', () => {
    expect(formatClock(0)).toBe('0:00');
    expect(formatClock(65.9)).toBe('1:05');
    expect(formatClock(3725)).toBe('1:02:05');
    expect(formatClock(Infinity)).toBe('--:--');
    expect(formatClock(NaN)).toBe('--:--');
    expect(formatClock(null)).toBe('--:--');
  });
});

describe('bounds', () => {
  const media = (duration: number, seekable: TimeRangesLike, buffered: TimeRangesLike, currentTime = 0) => ({ duration, seekable, buffered, currentTime });

  it('uses the duration when it is finite', () => {
    const b = boundsOf(media(120, ranges([0, 120]), ranges([0, 10])));
    expect(b).toEqual({ length: 120, max: 120, known: true });
    expect(fractionOf(30, b)).toBe(0.25);
    expect(timeAtFraction(0.8, b)).toBeCloseTo(96);
  });

  it('handles Infinity and NaN durations: seeking stays within what is known', () => {
    expect(boundsOf(media(Infinity, ranges([0, 7.5]), ranges([0, 4]), 2))).toEqual({ length: null, max: 7.5, known: false });
    expect(boundsOf(media(NaN, ranges(), ranges([0, 3])))).toEqual({ length: null, max: 3, known: false });
    expect(boundsOf(media(NaN, ranges(), ranges()))).toEqual({ length: null, max: 0, known: false });
    expect(fractionOf(5, boundsOf(media(NaN, ranges(), ranges())))).toBe(0);
  });

  it('clamps seeks into the known range', () => {
    const b = boundsOf(media(Infinity, ranges([0, 7.5]), ranges()));
    expect(seekTarget(7, 5, b)).toBe(7.5);
    expect(seekTarget(2, -5, b)).toBe(0);
    expect(rangesOf(ranges([1, 1], [2, 3]))).toEqual([{ start: 2, end: 3 }]);
  });
});

describe('keyboard map', () => {
  it('maps the shortcuts', () => {
    expect(keyAction(key(' '), true)).toEqual({ type: 'toggle' });
    expect(keyAction(key('k'), false)).toEqual({ type: 'toggle' });
    expect(keyAction(key('ArrowLeft'), false)).toEqual({ type: 'seekBy', seconds: -5 });
    expect(keyAction(key('ArrowRight'), false)).toEqual({ type: 'seekBy', seconds: 5 });
    expect(keyAction(key('ArrowLeft', { shiftKey: true }), false)).toEqual({ type: 'seekBy', seconds: -60 });
    expect(keyAction(key('ArrowRight', { shiftKey: true }), false)).toEqual({ type: 'seekBy', seconds: 60 });
    expect(keyAction(key('Home'), false)).toEqual({ type: 'seekStart' });
    expect(keyAction(key('End'), false)).toEqual({ type: 'seekEnd' });
    expect(keyAction(key('f'), false)).toEqual({ type: 'fullscreen' });
    expect(keyAction(key('m'), false)).toEqual({ type: 'mute' });
    expect(keyAction(key('x'), false)).toBeNull();
  });

  it('steps frames only while paused', () => {
    expect(keyAction(key(','), true)).toEqual({ type: 'frame', direction: -1 });
    expect(keyAction(key('.'), true)).toEqual({ type: 'frame', direction: 1 });
    expect(keyAction(key(','), false)).toBeNull();
    expect(keyAction(key('.'), false)).toBeNull();
    expect(FRAME_SECONDS).toBeGreaterThan(0);
  });

  it('leaves browser shortcuts alone', () => {
    expect(keyAction(key('ArrowRight', { ctrlKey: true }), false)).toBeNull();
    expect(keyAction(key('f', { metaKey: true }), false)).toBeNull();
    expect(keyAction(key('k', { altKey: true }), false)).toBeNull();
  });

  it('moves between speeds', () => {
    expect(nextSpeed(1, 1)).toBe(1.25);
    expect(nextSpeed(1, -1)).toBe(0.75);
    expect(nextSpeed(2, 1)).toBe(2);
    expect(nextSpeed(0.25, -1)).toBe(0.25);
  });
});

describe('step markers', () => {
  const steps = [
    step('open', 1000, 2000),
    step('pay', 3000, 5000, 'failed', [step('fill', 3200, 1000), step('submit', 4300, 3500, 'failed'), step('no time', null, 10)]),
    step('close', 8200, 300),
  ];

  it('places steps at start minus attempt start, nested ones included', () => {
    const m = buildMarkers(START, steps, 0);
    expect(m.map((x) => [x.name, x.time, x.depth])).toEqual([
      ['open', 1, 0],
      ['pay', 3, 0],
      ['fill', 3.2, 1],
      ['submit', 4.3, 1],
      ['close', 8.2, 0],
    ]);
    expect(m.find((x) => x.name === 'submit')?.end).toBeCloseTo(7.8);
  });

  it('places nothing when the attempt has no start, and skips steps without one', () => {
    expect(buildMarkers(null, steps, 0)).toEqual([]);
    expect(buildMarkers(START, [step('x', null, 5)], 0)).toEqual([]);
  });

  it('applies the nudge and never goes below zero', () => {
    const m = buildMarkers(START, steps, 500);
    expect(m[0]?.time).toBeCloseTo(1.5);
    expect(buildMarkers(START, [step('a', 100, 10)], -500)[0]?.time).toBe(0);
  });

  it('finds the failure moment in the deepest failed step', () => {
    expect(failureTime(buildMarkers(START, steps, 0))).toBeCloseTo(7.8);
    expect(failureTime(buildMarkers(START, [step('ok', 0, 100)], 0))).toBeNull();
  });

  it('follows playback with the innermost running step', () => {
    const m = buildMarkers(START, steps, 0);
    expect(currentMarker(m, 0.5)).toBeNull();
    expect(currentMarker(m, 1.5)?.name).toBe('open');
    expect(currentMarker(m, 3.1)?.name).toBe('pay');
    expect(currentMarker(m, 3.5)?.name).toBe('fill');
    expect(currentMarker(m, 4.5)?.name).toBe('submit');
    expect(currentMarker(m, 8.3)?.name).toBe('close');
    expect(currentMarker(m, 100)?.name).toBe('close');
  });

  it('keys the rows the same way as the markers', () => {
    const rows = flattenSteps(steps);
    expect(rows.map((r) => r.key)).toEqual(['0', '1', '1.0', '1.1', '1.2', '2']);
    expect(buildMarkers(START, steps, 0).map((x) => x.key)).toEqual(['0', '1', '1.0', '1.1', '2']);
  });
});

describe('offsets in the address', () => {
  it('round-trips and rejects junk', () => {
    expect(parseOffsets('a1:500,b2:-1500')).toEqual({ a1: 500, b2: -1500 });
    expect(serializeOffsets({ a1: 500, b2: -1500, c3: 0 })).toBe('a1:500,b2:-1500');
    expect(serializeOffsets({ a1: 0 })).toBeNull();
    expect(parseOffsets('x:abc,:5,y 1:5,ok:1000')).toEqual({ ok: 1000 });
    expect(parseOffsets(null)).toEqual({});
    expect(parseOffsets('a:999999999')).toEqual({ a: 30_000 });
  });

  it('rounds and clamps', () => {
    expect(clampOffset(-99_999)).toBe(-30_000);
    expect(clampOffset(12.4)).toBe(12);
    expect(formatOffset(0)).toBe('0.0 s');
    expect(formatOffset(1500)).toBe('+1.5 s');
    expect(formatOffset(-500)).toBe('-0.5 s');
  });
});

describe('media address refresh', () => {
  it('refreshes a minute before expiry, never sooner than the floor', () => {
    const now = 1_000_000;
    expect(refreshDelay(now + 15 * 60_000, now)).toBe(14 * 60_000);
    expect(refreshDelay(now + 30_000, now)).toBe(5_000);
    expect(refreshDelay(new Date(now + 10 * 60_000).toISOString(), now)).toBe(9 * 60_000);
    expect(refreshDelay('not a date', now)).toBe(10 * 60_000);
  });

  it('limits how often it can refresh', () => {
    expect(refreshAllowed([], 100)).toBe(true);
    expect(refreshAllowed([90, 95, 99], 100)).toBe(false);
    expect(refreshAllowed([1, 2, 3], 100_000)).toBe(true);
  });

  it('tells an expired address from an unsupported codec', () => {
    expect(classifyError(4, 404)).toBe('expired');
    expect(classifyError(4, 200)).toBe('unsupported');
    expect(classifyError(3, 206)).toBe('unsupported');
    expect(classifyError(2, null)).toBe('network');
  });

  it('keys thumbnails by whole seconds inside the bounds', () => {
    const b = { length: 10, max: 10, known: true };
    expect(thumbSecond(3.9, b)).toBe(3);
    expect(thumbSecond(99, b)).toBe(10);
    expect(thumbSecond(-4, b)).toBe(0);
  });
});

describe('remembered preferences', () => {
  it('survives broken or missing storage', () => {
    const store = new Map<string, string>();
    const storage = { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => void store.set(k, v) };
    savePrefs(storage, { speed: 1.5, volume: 0.4, muted: true });
    expect(loadPrefs(storage)).toEqual({ speed: 1.5, volume: 0.4, muted: true });
    store.set(PREFS_KEY, '{"speed":7,"volume":9}');
    expect(loadPrefs(storage)).toEqual({ speed: 1, volume: 1, muted: false });
    store.set(PREFS_KEY, 'not json');
    expect(loadPrefs(storage)).toEqual({ speed: 1, volume: 1, muted: false });
    const throwing = {
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('blocked');
      },
    };
    expect(loadPrefs(throwing)).toEqual({ speed: 1, volume: 1, muted: false });
    expect(() => savePrefs(throwing, { speed: 1, volume: 1, muted: false })).not.toThrow();
    expect(loadPrefs(null)).toEqual({ speed: 1, volume: 1, muted: false });
  });
});
