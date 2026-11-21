import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

export const ENABLED = process.env.FASTPROOF_E2E_REAL === '1';
export const PORT = Number(process.env.FASTPROOF_E2E_PORT ?? '18080');
export const BASE_URL = process.env.FASTPROOF_E2E_URL ?? `http://127.0.0.1:${String(PORT)}`;
export const WORK_DIR = process.env.FASTPROOF_E2E_DIR ?? join(tmpdir(), 'fastproof-e2e-real');
export const ADMIN_TOKEN = process.env.FASTPROOF_E2E_ADMIN_TOKEN ?? 'fpe2e-admin-token-for-tests-only-0001';
export const PROJECT = 'shop';
export const REPO = resolve(import.meta.dirname, '../..');
export const FIXTURES = join(REPO, 'tests/fixtures');
export const BIN = process.env.FASTPROOF_BIN ?? join(REPO, 'target/debug/fastproof');

/** Writes the static admin token file the server reads (`FASTPROOF_ADMIN_TOKENS_FILE`). */
export function adminTokensFile(): string {
  mkdirSync(WORK_DIR, { recursive: true });
  const file = join(WORK_DIR, 'admin-tokens');
  writeFileSync(file, `e2e-admin:admin:sha256:${createHash('sha256').update(ADMIN_TOKEN).digest('hex')}\n`);
  return file;
}

/** Writes a key file for project secrets (`FASTPROOF_SECRETS_KEY_FILE`), for tests only. */
export function secretsKeyFile(): string {
  mkdirSync(WORK_DIR, { recursive: true });
  const file = join(WORK_DIR, 'secrets-key');
  writeFileSync(file, `${createHash('sha256').update(`fastproof-e2e:${ADMIN_TOKEN}`).digest('hex')}\n`);
  return file;
}

export interface Seeded {
  viewerToken: string;
  /** Launch ids by name. */
  launches: Record<string, string>;
  /** A failed result of the newest finished main launch. */
  failedResultId: string;
  /** The test case of that result. */
  testCaseId: string;
  runningLaunchId: string;
  /** A failed result of the running launch, sent with live log lines. */
  liveResultId: string;
}

export function seeded(): Seeded {
  return JSON.parse(readFileSync(join(WORK_DIR, 'seeded.json'), 'utf8')) as Seeded;
}

export function saveSeeded(s: Seeded): void {
  writeFileSync(join(WORK_DIR, 'seeded.json'), JSON.stringify(s, null, 2));
}
