import { expect, test as base } from '@playwright/test';

/** Lets a test accept console errors it provokes on purpose. */
export interface ConsoleGuard {
  allow: (pattern: RegExp) => void;
}

/**
 * A `test` that fails when the page logs a console error (React key and prop warnings included) or throws an
 * uncaught exception. A test that provokes an error on purpose calls `consoleGuard.allow(pattern)`.
 */
export const test = base.extend<{ consoleGuard: ConsoleGuard }>({
  consoleGuard: [
    async ({ page }, provide) => {
      const errors: string[] = [];
      const allowed: RegExp[] = [];
      page.on('console', (m) => {
        if (m.type() === 'error') errors.push(`console: ${m.text()} (${m.location().url})`);
      });
      page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
      await provide({ allow: (p) => allowed.push(p) });
      const unexpected = errors.filter((e) => !allowed.some((r) => r.test(e)));
      expect(unexpected, 'console errors on the page').toEqual([]);
    },
    { auto: true },
  ],
});

export { expect };
