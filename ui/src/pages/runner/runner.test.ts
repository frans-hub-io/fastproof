import { describe, expect, it } from 'vitest';
import { editorAction, runnerAction } from './keymap';
import { SaveQueue, type QueueSnapshot } from './saveQueue';

const on = { singleKeys: true, typing: false };

describe('runner keyboard map', () => {
  it('maps every key of the spec', () => {
    const cases: [Parameters<typeof runnerAction>[0], string][] = [
      [{ key: 'j' }, 'nextStep'],
      [{ key: 'ArrowDown' }, 'nextStep'],
      [{ key: 'k' }, 'prevStep'],
      [{ key: 'ArrowUp' }, 'prevStep'],
      [{ key: 'c' }, 'comment'],
      [{ key: 'a' }, 'attach'],
      [{ key: 'P', shiftKey: true }, 'passRest'],
      [{ key: 'Enter' }, 'enter'],
      [{ key: 'n' }, 'nextCase'],
      [{ key: 'N', shiftKey: true }, 'prevCase'],
      [{ key: 'd' }, 'defect'],
      [{ key: 'Enter', ctrlKey: true }, 'finish'],
      [{ key: 'Enter', metaKey: true }, 'finish'],
      [{ key: '?', shiftKey: true }, 'help'],
      [{ key: 'Escape' }, 'escape'],
    ];
    for (const [k, type] of cases) expect(runnerAction(k, on)?.type, JSON.stringify(k)).toBe(type);
    expect(runnerAction({ key: 'p' }, on)).toEqual({ type: 'status', status: 'passed' });
    expect(runnerAction({ key: 'f' }, on)).toEqual({ type: 'status', status: 'failed' });
    expect(runnerAction({ key: 'b' }, on)).toEqual({ type: 'status', status: 'blocked' });
    expect(runnerAction({ key: 's' }, on)).toEqual({ type: 'status', status: 'skipped' });
    expect(runnerAction({ key: 'u' }, on)).toEqual({ type: 'status', status: 'untested' });
  });

  it('ignores keys with modifiers and caps-lock capitals without Shift', () => {
    expect(runnerAction({ key: 'p', ctrlKey: true }, on)).toBeNull();
    expect(runnerAction({ key: 'p', altKey: true }, on)).toBeNull();
    expect(runnerAction({ key: 'P' }, on)).toBeNull();
    expect(runnerAction({ key: 'x' }, on)).toBeNull();
  });

  it('only Ctrl+Enter and Escape work while typing', () => {
    const typing = { singleKeys: true, typing: true };
    expect(runnerAction({ key: 'p' }, typing)).toBeNull();
    expect(runnerAction({ key: 'Enter' }, typing)).toBeNull();
    expect(runnerAction({ key: 'Enter', ctrlKey: true }, typing)?.type).toBe('finish');
    expect(runnerAction({ key: 'Escape' }, typing)?.type).toBe('escape');
  });

  it('turning single-key shortcuts off keeps arrows, Enter and Ctrl+Enter', () => {
    const off = { singleKeys: false, typing: false };
    expect(runnerAction({ key: 'p' }, off)).toBeNull();
    expect(runnerAction({ key: 'j' }, off)).toBeNull();
    expect(runnerAction({ key: '?' }, off)).toBeNull();
    expect(runnerAction({ key: 'ArrowDown' }, off)?.type).toBe('nextStep');
    expect(runnerAction({ key: 'Enter' }, off)?.type).toBe('enter');
    expect(runnerAction({ key: 'Enter', ctrlKey: true }, off)?.type).toBe('finish');
  });
});

describe('case editor keys', () => {
  it('saves anywhere, edits steps only inside a step field', () => {
    expect(editorAction({ key: 's', ctrlKey: true }, { inStep: false, released: false })).toEqual({ type: 'save' });
    expect(editorAction({ key: 'Enter', ctrlKey: true }, { inStep: false, released: false })).toBeNull();
    expect(editorAction({ key: 'Enter', ctrlKey: true }, { inStep: true, released: false })).toEqual({ type: 'newStepBelow' });
    expect(editorAction({ key: 'ArrowUp', altKey: true }, { inStep: true, released: false })).toEqual({ type: 'moveStep', dir: -1 });
    expect(editorAction({ key: 'ArrowDown', altKey: true }, { inStep: true, released: false })).toEqual({ type: 'moveStep', dir: 1 });
    expect(editorAction({ key: 'Tab' }, { inStep: true, released: false })).toEqual({ type: 'indent' });
    expect(editorAction({ key: 'Tab', shiftKey: true }, { inStep: true, released: false })).toEqual({ type: 'outdent' });
  });

  it('Escape releases Tab so focus can leave (no keyboard trap)', () => {
    expect(editorAction({ key: 'Escape' }, { inStep: true, released: false })).toEqual({ type: 'release' });
    expect(editorAction({ key: 'Tab' }, { inStep: true, released: true })).toBeNull();
  });
});

describe('save queue', () => {
  function manualTimers() {
    const pending: (() => void)[] = [];
    return {
      opts: { setTimer: (fn: () => void) => pending.push(fn), clearTimer: () => undefined, backoff: () => 0 },
      fire: () => pending.splice(0).forEach((f) => f()),
    };
  }

  const tick = () => new Promise((r) => setTimeout(r, 0));

  it('runs writes in order and coalesces waiting writes of the same key', async () => {
    const order: string[] = [];
    let release: () => void = () => undefined;
    const q = new SaveQueue();
    q.enqueue('1', () => new Promise<void>((r) => (release = () => (order.push('1a'), r()))));
    q.enqueue('2', () => (order.push('2a'), Promise.resolve()));
    q.enqueue('2', () => (order.push('2b'), Promise.resolve()));
    q.enqueue('1', () => (order.push('1b'), Promise.resolve()));
    expect(q.pending).toBe(3);
    release();
    await q.drain();
    expect(order).toEqual(['1a', '2b', '1b']);
  });

  it('retries a failed write and keeps the writes behind it', async () => {
    const t = manualTimers();
    const q = new SaveQueue(t.opts);
    const states: QueueSnapshot[] = [];
    q.subscribe((s) => states.push(s));
    let fail = 2;
    const done: string[] = [];
    q.enqueue('1', () => {
      if (fail-- > 0) return Promise.reject(new Error('offline'));
      done.push('1');
      return Promise.resolve();
    });
    q.enqueue('2', () => (done.push('2'), Promise.resolve()));
    await tick();
    expect(q.snapshot()).toMatchObject({ state: 'retrying', pending: 2, lastError: 'offline' });
    t.fire();
    await tick();
    expect(done).toEqual([]);
    t.fire();
    await tick();
    await tick();
    expect(done).toEqual(['1', '2']);
    expect(q.snapshot()).toMatchObject({ state: 'saved', pending: 0, lastError: null });
    expect(states.some((s) => s.state === 'retrying')).toBe(true);
  });

  it('a newer write replaces the failed one waiting for retry', async () => {
    const t = manualTimers();
    const q = new SaveQueue(t.opts);
    const seen: string[] = [];
    q.enqueue('1', () => Promise.reject(new Error('500')));
    await tick();
    q.enqueue('1', () => (seen.push('newer'), Promise.resolve()));
    q.retryNow();
    await q.drain();
    expect(seen).toEqual(['newer']);
  });
});
