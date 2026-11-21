const PLACEHOLDER = '__CSP_NONCE__';

let cached: string | undefined | null = null;

/**
 * The per-response CSP nonce the server writes into `<meta name="csp-nonce">` of `index.html`;
 * `undefined` when the page was not served with one (Vite dev server, tests).
 */
export function cspNonce(): string | undefined {
  if (cached !== null) return cached;
  const value = typeof document === 'undefined' ? null : document.querySelector<HTMLMetaElement>('meta[name="csp-nonce"]')?.content;
  cached = value && value !== PLACEHOLDER && /^[A-Za-z0-9+/=_-]{8,128}$/.test(value) ? value : undefined;
  return cached;
}

export function resetCspNonceCache(): void {
  cached = null;
}
