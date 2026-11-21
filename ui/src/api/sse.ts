import { getToken } from '../lib/authToken';

export type StreamReadyState = 'connecting' | 'open' | 'closed';

export interface EventStream {
  close: () => void;
  readyState: () => StreamReadyState;
}

export interface StreamHandlers {
  onOpen?: () => void;
  /** A connection failed or dropped; `closed` when no reconnect follows. */
  onError?: (closed: boolean) => void;
  /** Handlers by event name. */
  events: Record<string, (data: string) => void>;
}

export function eventStreamSupported(): boolean {
  return typeof EventSource !== 'undefined' || (typeof fetch !== 'undefined' && typeof ReadableStream !== 'undefined');
}

/**
 * Server-Sent Events from a same-origin URL. With a cookie session it is a plain `EventSource`;
 * signed in with an API token it reads the stream with `fetch` (which carries the token header,
 * see `installTokenAuth`) and reconnects like `EventSource` does.
 */
export function openEventStream(url: string, h: StreamHandlers): EventStream {
  if (!getToken() && typeof EventSource !== 'undefined') return nativeStream(url, h);
  return fetchStream(url, h);
}

function nativeStream(url: string, h: StreamHandlers): EventStream {
  const es = new EventSource(url, { withCredentials: true });
  es.onopen = () => h.onOpen?.();
  es.onerror = () => h.onError?.(es.readyState === EventSource.CLOSED);
  for (const [name, fn] of Object.entries(h.events)) {
    es.addEventListener(name, (e: MessageEvent<string>) => fn(e.data));
  }
  return {
    close: () => es.close(),
    readyState: () => (es.readyState === EventSource.OPEN ? 'open' : es.readyState === EventSource.CLOSED ? 'closed' : 'connecting'),
  };
}

const MAX_EVENT_BYTES = 8 * 1024 * 1024;

function fetchStream(url: string, h: StreamHandlers): EventStream {
  let state: StreamReadyState = 'connecting';
  let stopped = false;
  let retryMs = 3000;
  let controller: AbortController | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const dispatch = (event: string, data: string[]) => {
    if (!data.length) return;
    h.events[event || 'message']?.(data.join('\n'));
  };

  const run = async () => {
    controller = new AbortController();
    state = 'connecting';
    try {
      const res = await fetch(url, { headers: { Accept: 'text/event-stream' }, cache: 'no-store', signal: controller.signal });
      if (!res.ok || !res.body) {
        state = 'closed';
        stopped = true;
        h.onError?.(true);
        return;
      }
      state = 'open';
      h.onOpen?.();
      const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
      let buf = '';
      let event = '';
      let data: string[] = [];
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += value;
        if (buf.length > MAX_EVENT_BYTES) throw new Error('event too large');
        let nl: number;
        while ((nl = buf.search(/\r\n|\r|\n/)) >= 0) {
          const line = buf.slice(0, nl);
          buf = buf.slice(nl + (buf.startsWith('\r\n', nl) ? 2 : 1));
          if (line === '') {
            dispatch(event, data);
            event = '';
            data = [];
          } else if (!line.startsWith(':')) {
            const i = line.indexOf(':');
            const field = i < 0 ? line : line.slice(0, i);
            const val = i < 0 ? '' : line.slice(i + 1).replace(/^ /, '');
            if (field === 'event') event = val;
            else if (field === 'data') data.push(val);
            else if (field === 'retry' && /^\d+$/.test(val)) retryMs = Math.min(60_000, Math.max(500, Number(val)));
          }
        }
      }
    } catch {
      if (stopped) return;
    }
    if (stopped) return;
    state = 'connecting';
    h.onError?.(false);
    timer = setTimeout(() => void run(), retryMs);
  };

  void run();
  return {
    close: () => {
      stopped = true;
      state = 'closed';
      clearTimeout(timer);
      controller?.abort();
    },
    readyState: () => state,
  };
}
