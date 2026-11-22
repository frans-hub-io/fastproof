const THUMB_WIDTH = 176;
const MAX_CACHED = 160;
const MIN_GAP_MS = 90;
const SEEK_TIMEOUT_MS = 2000;

export function canGenerateThumbnails(): boolean {
  if (typeof window === 'undefined' || typeof document === 'undefined') return false;
  try {
    if (window.matchMedia('(hover: none), (pointer: coarse)').matches) return false;
  } catch {
    return false;
  }
  return typeof HTMLCanvasElement !== 'undefined';
}

/**
 * Frames for the hover preview of the seek bar. A second video element on the same address seeks to
 * the hovered second and draws it to a canvas; one seek at a time, the newest request wins, and only
 * the latest few frames are kept.
 */
export class ThumbnailSource {
  private video: HTMLVideoElement | null = null;
  private readonly cache = new Map<number, string>();
  private pending: number | null = null;
  private running = false;
  private disposed = false;
  private lastSeekAt = 0;
  private broken = false;

  constructor(
    private src: string,
    private readonly onFrame: (second: number) => void,
  ) {}

  get usable(): boolean {
    return !this.broken && !this.disposed;
  }

  setSrc(src: string): void {
    if (src === this.src) return;
    this.src = src;
    if (this.video) {
      this.video.src = src;
      this.video.load();
    }
  }

  get(second: number): string | undefined {
    return this.cache.get(second);
  }

  /** The cached frame closest to `second`, to show while the exact one is made. */
  nearest(second: number): string | undefined {
    let best: number | null = null;
    for (const k of this.cache.keys()) if (best === null || Math.abs(k - second) < Math.abs(best - second)) best = k;
    return best === null ? undefined : this.cache.get(best);
  }

  get size(): number {
    return this.cache.size;
  }

  request(second: number): void {
    if (!this.usable || this.cache.has(second)) return;
    this.pending = second;
    if (!this.running) void this.run();
  }

  dispose(): void {
    this.disposed = true;
    this.cache.clear();
    if (this.video) {
      this.video.removeAttribute('src');
      this.video.load();
      this.video = null;
    }
  }

  private element(): HTMLVideoElement {
    if (!this.video) {
      const v = document.createElement('video');
      v.muted = true;
      v.preload = 'auto';
      v.playsInline = true;
      v.setAttribute('aria-hidden', 'true');
      v.src = this.src;
      this.video = v;
    }
    return this.video;
  }

  private async run(): Promise<void> {
    this.running = true;
    try {
      while (this.pending !== null && this.usable) {
        const wait = this.lastSeekAt + MIN_GAP_MS - Date.now();
        if (wait > 0) await new Promise((r) => setTimeout(r, wait));
        const second: number = this.pending;
        this.pending = null;
        if (this.cache.has(second)) continue;
        this.lastSeekAt = Date.now();
        const url = await this.capture(second);
        if (url && !this.disposed) {
          this.cache.set(second, url);
          while (this.cache.size > MAX_CACHED) {
            const oldest = this.cache.keys().next().value;
            if (oldest === undefined) break;
            this.cache.delete(oldest);
          }
          this.onFrame(second);
        }
      }
    } finally {
      this.running = false;
    }
  }

  private capture(second: number): Promise<string | null> {
    const v = this.element();
    return new Promise((resolve) => {
      let done = false;
      const finish = (value: string | null) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        v.removeEventListener('seeked', onSeeked);
        v.removeEventListener('error', onError);
        resolve(value);
      };
      const draw = () => {
        try {
          if (!v.videoWidth || !v.videoHeight) return finish(null);
          const canvas = document.createElement('canvas');
          canvas.width = THUMB_WIDTH;
          canvas.height = Math.max(1, Math.round((THUMB_WIDTH * v.videoHeight) / v.videoWidth));
          const ctx = canvas.getContext('2d');
          if (!ctx) {
            this.broken = true;
            return finish(null);
          }
          ctx.drawImage(v, 0, 0, canvas.width, canvas.height);
          finish(canvas.toDataURL('image/jpeg', 0.6));
        } catch {
          this.broken = true;
          finish(null);
        }
      };
      const onSeeked = () => draw();
      const onError = () => {
        this.broken = true;
        finish(null);
      };
      const timer = setTimeout(() => finish(null), SEEK_TIMEOUT_MS);
      v.addEventListener('seeked', onSeeked);
      v.addEventListener('error', onError);
      const start = () => {
        const seekable = v.seekable;
        if (!seekable.length) return finish(null);
        const target = Math.min(second, seekable.end(seekable.length - 1));
        if (Math.abs(v.currentTime - target) < 0.01 && v.readyState >= 2) return draw();
        v.currentTime = target;
      };
      if (v.readyState >= 1) start();
      else v.addEventListener('loadedmetadata', start, { once: true });
    });
  }
}
