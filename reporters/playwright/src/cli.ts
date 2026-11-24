#!/usr/bin/env node
import { Api } from './api';
import { resolveConfig } from './config';
import { PROJECT_KEY, flushSpool, listSessions } from './spool';

const USAGE = `usage: fastproof-playwright flush [--spool DIR] [--timeout SECONDS]
       fastproof-playwright list [--spool DIR]

Sends results that earlier runs left in the spool (server down, flush timeout).
Reads FASTPROOF_URL and FASTPROOF_TOKEN (or FASTPROOF_TOKEN_FILE). FASTPROOF_PROJECT limits the flush to one
project; without it each session goes to the project it was recorded for.`;

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
  if (!cfg.url) {
    process.stderr.write('FASTPROOF_URL is required\n');
    return 2;
  }
  const projects = cfg.project
    ? [cfg.project]
    : [...new Set(listSessions(cfg.spoolDir).map((s) => s.meta.project).filter((p): p is string => typeof p === 'string' && PROJECT_KEY.test(p)))].sort();
  const timeout = Number(opts.timeout ?? '600');
  const deadline = Date.now() + timeout * 1000;
  const r = { sessions: 0, results: 0, failed: 0, remaining: 0, errors: [] as string[] };
  for (const project of projects) {
    const api = new Api(cfg.url, cfg.token, project, cfg.requestTimeoutMs);
    const one = await flushSpool(api, cfg.spoolDir, { deadline, project });
    r.sessions += one.sessions;
    r.results += one.results;
    r.failed += one.failed;
    r.remaining += one.remaining;
    r.errors.push(...one.errors);
  }
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
