import { closeSync, openSync, readSync, statSync } from 'node:fs';
import { join } from 'node:path';

import { shortHash } from '@franshub/fastproof-playwright';

/** Environment variable with the directory the reporter and the test processes share for one run. */
export const STAGE_ENV = 'FASTPROOF_JEST_STAGE';

export interface StageLog {
  k: 'log';
  /** milliseconds since the epoch */
  t: number;
  s: 'stdout' | 'stderr' | 'step';
  l: 'debug' | 'info' | 'warn' | 'error';
  m: string;
  step?: string;
}

export interface StageAttachment {
  k: 'att';
  n: string;
  c: string;
  p: string;
}

export type StageLine = StageLog | StageAttachment;

/** The test a hook belongs to: file and full name give the same key in the reporter and in the test process. */
export function testKey(testPath: string, fullName: string): string {
  return `${testPath}\u0000${fullName}`;
}

export function attemptFile(dir: string, key: string, attempt: number): string {
  return join(dir, `${shortHash(key)}-${attempt}.ndjson`);
}

const MAX_READ = 8 * 1024 * 1024;

/** Reads the lines appended to an attempt's file since the last call; a partial last line waits. */
export class Tail {
  private offset = 0;
  private rest = '';
  constructor(readonly path: string) {}

  read(): StageLine[] {
    let size: number;
    try {
      size = statSync(this.path).size;
    } catch {
      return [];
    }
    if (size <= this.offset) return [];
    const len = Math.min(size - this.offset, MAX_READ);
    const buf = Buffer.alloc(len);
    const fd = openSync(this.path, 'r');
    try {
      readSync(fd, buf, 0, len, this.offset);
    } finally {
      closeSync(fd);
    }
    this.offset += len;
    const text = this.rest + buf.toString('utf8');
    const parts = text.split('\n');
    this.rest = parts.pop() ?? '';
    const out: StageLine[] = [];
    for (const p of parts) {
      if (!p) continue;
      try {
        const o = JSON.parse(p) as StageLine;
        if (o && (o.k === 'log' || o.k === 'att')) out.push(o);
      } catch {
        continue;
      }
    }
    return out;
  }

  /** Everything written so far, including the end of a last line without a newline. */
  drain(): StageLine[] {
    const out: StageLine[] = [];
    for (let i = 0; i < 16; i++) {
      const before = this.offset;
      out.push(...this.read());
      if (this.offset === before) break;
    }
    if (this.rest) {
      try {
        const o = JSON.parse(this.rest) as StageLine;
        if (o && (o.k === 'log' || o.k === 'att')) out.push(o);
      } catch {
        /* partial line of a killed process */
      }
      this.rest = '';
    }
    return out;
  }
}
