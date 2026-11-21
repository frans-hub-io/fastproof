import { expect, type Page } from '@playwright/test';

export interface Allowed {
  /** API path pattern and the status it may answer with. */
  path: RegExp;
  status: number;
}

/**
 * Collects what must not happen on a page: console errors, uncaught exceptions, CSP violations
 * and failing same-origin requests other than the allowed ones.
 */
export class Watcher {
  readonly problems: string[] = [];
  private allowed: Allowed[] = [];

  constructor(page: Page, origin: string) {
    page.on('console', (m) => {
      if (m.type() !== 'error') return;
      const text = m.text();
      if (text.startsWith('Failed to load resource')) return;
      this.problems.push(`console: ${text.slice(0, 160)} @ ${new URL(page.url()).pathname}`);
    });
    page.on('pageerror', (e) => this.problems.push(`exception: ${e.message}`));
    page.on('response', (r) => {
      const url = new URL(r.url());
      if (url.origin !== origin || r.status() < 400) return;
      if (this.allowed.some((a) => a.status === r.status() && a.path.test(url.pathname))) return;
      this.problems.push(`HTTP ${String(r.status())} ${r.request().method()} ${url.pathname}${url.search} @ ${new URL(page.url()).pathname}`);
    });
    page.on('requestfailed', (r) => {
      const url = new URL(r.url());
      const why = r.failure()?.errorText ?? '';
      if (url.origin !== origin || why.includes('ERR_ABORTED')) return;
      this.problems.push(`failed: ${r.method()} ${url.pathname} ${why}`);
    });
    void page.addInitScript(() => {
      document.addEventListener('securitypolicyviolation', (e) => {
        console.error(`CSP violation: ${e.violatedDirective} ${e.blockedURI}`);
      });
    });
  }

  allow(...a: Allowed[]): void {
    this.allowed.push(...a);
  }

  check(): void {
    expect(this.problems, this.problems.join('\n')).toEqual([]);
  }
}

/** Opens the app signed in with an API token, as the sign-in page leaves it. */
export async function signInWithToken(page: Page, token: string): Promise<void> {
  await page.addInitScript((t) => {
    try {
      window.sessionStorage.setItem('fastproof.apiToken', t);
    } catch {
      /* storage unavailable */
    }
  }, token);
}
