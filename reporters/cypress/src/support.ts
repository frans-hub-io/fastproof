/**
 * Browser side of the FastProof Cypress plugin. Import it from `cypress/support/e2e.js`:
 *
 *     import '@franshub/fastproof-cypress/support';
 *
 * It tells the plugin (node process) when each attempt of a test begins and ends, with the command log of the
 * attempt, through `cy.task`. Nothing here talks to FastProof. It needs no Node modules, so Cypress can bundle it.
 */

interface Trace {
  traceparent: string;
  traceId: string;
  spanId: string;
  baggage: string;
}

interface Begin extends Trace {
  skip?: boolean;
}

interface LogLine {
  t: number;
  m: string;
  l?: 'info' | 'error';
}

interface Current {
  trace: Trace | undefined;
  logs: LogLine[];
}

const MAX_LOG_LINES = 2000;
const MAX_LOG_CHARS = 2000;
const IGNORED_COMMANDS = new Set(['task', 'then', 'wrap', 'should', 'and', 'within', 'wait', 'log']);

const g = globalThis as unknown as {
  Cypress?: any;
  cy?: any;
  beforeEach?: (fn: (this: any) => void) => void;
  afterEach?: (fn: (this: any) => void) => void;
};

let current: Current = { trace: undefined, logs: [] };

function push(line: LogLine): void {
  if (current.logs.length < MAX_LOG_LINES) current.logs.push({ ...line, m: line.m.slice(0, MAX_LOG_CHARS) });
}

function show(v: unknown): string {
  if (typeof v === 'string') return v;
  try {
    return JSON.stringify(v) ?? String(v);
  } catch {
    return String(v);
  }
}

/** The trace context of the running attempt, as the plugin reported it with the result. */
export function fastproofTrace(): Trace & { headers(opts?: { newSpan?: boolean }): Record<string, string> } {
  const t = current.trace ?? { traceparent: '', traceId: '', spanId: '', baggage: '' };
  return {
    ...t,
    headers(opts) {
      if (!t.traceparent) return {} as Record<string, string>;
      let tp = t.traceparent;
      if (opts?.newSpan) {
        let span = '';
        for (let i = 0; i < 16; i++) span += Math.floor(Math.random() * 16).toString(16);
        if (/^0+$/.test(span)) span = span.slice(0, 15) + '1';
        tp = `00-${t.traceId}-${span}-01`;
      }
      return { traceparent: tp, baggage: t.baggage };
    },
  };
}

function install(): void {
  const C = g.Cypress;
  if (!C || !g.cy || !g.beforeEach || !g.afterEach) return;
  const titlePath = (): string[] => (C.currentTest?.titlePath ?? []).map(String);

  C.on('command:start', (cmd: any) => {
    const a = cmd?.attributes ?? cmd;
    const name = String(a?.name ?? '');
    if (!name || IGNORED_COMMANDS.has(name)) return;
    const args = Array.isArray(a?.args) ? a.args.slice(0, 3).map(show).join(', ') : '';
    push({ t: Date.now(), m: `${name}${args ? ` ${args}` : ''}` });
  });
  C.on('log:added', (attrs: any) => {
    if (attrs?.name === 'log' || attrs?.name === 'fastproof') push({ t: Date.now(), m: String(attrs?.message ?? '') });
  });

  g.beforeEach(function (this: any) {
    current = { trace: undefined, logs: [] };
    const spec = String(C.spec?.relative ?? C.spec?.name ?? '');
    g.cy
      .task('fastproof:begin', { spec, titlePath: titlePath(), retry: Number(C.currentRetry ?? 0), start: Date.now() }, { log: false })
      .then((r: Begin | null) => {
        if (!r) return;
        current.trace = { traceparent: r.traceparent, traceId: r.traceId, spanId: r.spanId, baggage: r.baggage };
        if (r.skip) this.skip();
      });
  });

  g.afterEach(function (this: any) {
    const t = this.currentTest;
    const spec = String(C.spec?.relative ?? C.spec?.name ?? '');
    const err = t?.err ? { name: String(t.err.name ?? ''), message: String(t.err.message ?? ''), stack: String(t.err.stack ?? '') } : undefined;
    const logs = current.logs;
    current = { trace: current.trace, logs: [] };
    g.cy.task(
      'fastproof:end',
      { spec, titlePath: titlePath(), retry: Number(C.currentRetry ?? 0), state: t?.state ?? 'unknown', err, logs, stop: Date.now() },
      { log: false },
    );
  });
}

install();
