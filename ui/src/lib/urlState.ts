import { useCallback, useLayoutEffect, useRef, useState } from 'react';
import { useSearchParams } from 'react-router';

/**
 * The page's search parameters with the changes made here applied at once. The router commits
 * a navigation in a transition, so a second quick change (a second checkbox, a second label)
 * would otherwise start from the old URL and drop the first one.
 */
export function useLatestSearchParams(): [URLSearchParams, (changes: Record<string, string | null | undefined>) => void] {
  const [sp, setSp] = useSearchParams();
  const url = sp.toString();
  const [pending, setPending] = useState<{ base: string; next: string } | null>(null);
  if (pending && pending.base !== url) setPending(null);
  const current = pending && pending.base === url ? pending.next : url;
  const latest = useRef({ base: url, next: current });
  useLayoutEffect(() => {
    latest.current = { base: url, next: current };
  });
  const update = useCallback(
    (changes: Record<string, string | null | undefined>) => {
      const next = new URLSearchParams(latest.current.next);
      for (const [k, v] of Object.entries(changes)) {
        if (v) next.set(k, v);
        else next.delete(k);
      }
      const value = { base: latest.current.base, next: next.toString() };
      latest.current = value;
      setPending(value);
      setSp(next, { replace: true });
    },
    [setSp],
  );
  return [new URLSearchParams(current), update];
}
