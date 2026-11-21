#!/usr/bin/env node
import { Api } from './api';
import { resolveConfig } from './config';
import { flushSpool, listSessions } from './spool';

const USAGE = `usage: fastproof-playwright flush [--spool DIR] [--timeout SECONDS]
       fastproof-playwright list [--spool DIR]

Sends results that earlier runs left in the spool (server down, flush timeout).
Reads FASTPROOF_URL, FASTPROOF_TOKEN (or FASTPROOF_TOKEN_FILE) and FASTPROOF_PROJECT.`;

async function main(argv: string[]): Promise<number> {
  const [cmd, ...rest] = argv;
  const opts: Record<string, string> = {};
  for (let i = 0; i < rest.length; i++) {
    const a = rest[i]!;
    if (a.startsWith('--')) opts[a.slice(2)] = rest[++i] ?? '';
  }
  if (cmd !== 'flush' && cmd !== 'list') {
    process.stdout.write(USAGE + '\n');
    return cmd === undefined || cmd === '--help' || cmd === '-h' ? 0 : 2;
  }
  const cfg = resolveConfig(opts.spool ? { spoolDir: opts.spool } : {});
  if (cmd === 'list') {
    for (const s of listSessions(cfg.spoolDir))
      process.stdout.write(`${s.id}\t${s.meta.project}\t${s.meta.launch.externalId}\t${s.meta.ended ? 'ended' : 'interrupted'}\n`);
    return 0;
  }
  if (!cfg.url || !cfg.project) {
    process.stderr.write('FASTPROOF_URL and FASTPROOF_PROJECT are required\n');
    return 2;
  }
  const api = new Api(cfg.url, cfg.token, cfg.project, cfg.requestTimeoutMs);
  const timeout = Number(opts.timeout ?? '600');
  const r = await flushSpool(api, cfg.spoolDir, { deadline: Date.now() + timeout * 1000, project: cfg.project });
  process.stdout.write(`sent ${r.results} results from ${r.sessions} sessions; ${r.remaining} left, ${r.failed} rejected\n`);
  for (const e of r.errors.slice(0, 5)) process.stderr.write(`  ${e}\n`);
  return r.remaining ? 1 : 0;
}

main(process.argv.slice(2)).then(
  (code) => process.exit(code),
  (e) => {
    process.stderr.write(`${String(e)}\n`);
    process.exit(1);
  },
);
