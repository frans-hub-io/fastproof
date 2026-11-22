export type SaveState = 'idle' | 'saving' | 'retrying' | 'saved';

export interface QueueSnapshot {
  state: SaveState;
  pending: number;
  lastError: string | null;
}

interface Task {
  key: string;
  run: () => Promise<void>;
  attempts: number;
}

export interface SaveQueueOptions {
  /** Delay before retry number `n` (1-based). */
  backoff?: (n: number) => number;
  setTimer?: (fn: () => void, ms: number) => unknown;
  clearTimer?: (t: unknown) => void;
}

const defaultBackoff = (n: number) => Math.min(30_000, 1000 * 2 ** (n - 1));

/**
 * The runner's in-memory write queue: writes run one at a time in order, a newer write with the same key replaces a
 * waiting one (last writer wins per step), and a failed write is retried with backoff until it succeeds, keeping
 * the writes queued behind it.
 */
export class SaveQueue {
  private tasks: Task[] = [];
  private running = false;
  private timer: unknown = null;
  private lastError: string | null = null;
  private listeners = new Set<(s: QueueSnapshot) => void>();
  private saved = false;
  private readonly backoff: (n: number) => number;
  private readonly setTimer: (fn: () => void, ms: number) => unknown;
  private readonly clearTimer: (t: unknown) => void;

  constructor(o: SaveQueueOptions = {}) {
    this.backoff = o.backoff ?? defaultBackoff;
    this.setTimer = o.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
    this.clearTimer = o.clearTimer ?? ((t) => clearTimeout(t as ReturnType<typeof setTimeout>));
  }

  subscribe(fn: (s: QueueSnapshot) => void): () => void {
    this.listeners.add(fn);
    fn(this.snapshot());
    return () => this.listeners.delete(fn);
  }

  snapshot(): QueueSnapshot {
    const state: SaveState = this.tasks.length === 0 ? (this.saved ? 'saved' : 'idle') : this.lastError ? 'retrying' : 'saving';
    return { state, pending: this.tasks.length, lastError: this.lastError };
  }

  private emit(): void {
    const s = this.snapshot();
    for (const l of this.listeners) l(s);
  }

  enqueue(key: string, run: () => Promise<void>): void {
    const waiting = this.tasks.findIndex((t, i) => t.key === key && (i > 0 || !this.running));
    if (waiting >= 0) this.tasks[waiting] = { key, run, attempts: 0 };
    else this.tasks.push({ key, run, attempts: 0 });
    this.emit();
    if (!this.running && this.timer === null) void this.pump();
  }

  /** Retries now instead of waiting for the backoff. */
  retryNow(): void {
    if (this.timer !== null) {
      this.clearTimer(this.timer);
      this.timer = null;
    }
    if (!this.running) void this.pump();
  }

  /** Resolves when every queued write has been saved. */
  async drain(): Promise<void> {
    if (!this.tasks.length) return;
    await new Promise<void>((resolve) => {
      const off = this.subscribe((s) => {
        if (s.pending === 0) {
          off();
          resolve();
        }
      });
    });
  }

  get pending(): number {
    return this.tasks.length;
  }

  private async pump(): Promise<void> {
    this.running = true;
    while (this.tasks.length) {
      const task = this.tasks[0];
      if (!task) break;
      try {
        await task.run();
        this.tasks.shift();
        this.lastError = null;
        this.saved = true;
        this.emit();
      } catch (e) {
        task.attempts++;
        this.lastError = e instanceof Error ? e.message : String(e);
        this.running = false;
        this.emit();
        this.timer = this.setTimer(() => {
          this.timer = null;
          void this.pump();
        }, this.backoff(task.attempts));
        return;
      }
    }
    this.running = false;
    this.emit();
  }

  dispose(): void {
    if (this.timer !== null) this.clearTimer(this.timer);
    this.timer = null;
    this.tasks = [];
    this.listeners.clear();
  }
}
