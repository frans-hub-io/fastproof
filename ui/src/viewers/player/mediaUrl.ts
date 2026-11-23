import { useCallback, useEffect, useRef, useState } from 'react';
import { errorMessage, projectPath, request } from '../../api/client';
import { refreshAllowed, refreshDelay } from './playerMath';

export interface MediaUrl {
  url: string;
  expiresAt: string;
}

export async function requestMediaUrl(project: string, id: string): Promise<MediaUrl> {
  const m = await request<MediaUrl>(`${projectPath(project)}/attachments/${encodeURIComponent(id)}/media-url`, { method: 'POST' });
  if (typeof m.url !== 'string' || !m.url.startsWith('/') || m.url.startsWith('//')) throw new Error('the server returned an invalid media address');
  return m;
}

const RETRY_MS = 30_000;

export interface MediaSource {
  src: string | undefined;
  error: string | null;
  /** Ask for a new address now (for example after a 404). `false` when refreshing too often or failing. */
  refresh: () => Promise<boolean>;
}

/**
 * A short-lived media address for one attachment, replaced a minute before it expires. The element
 * keeps playing from the old address until the new one is set.
 */
export function useMediaSource(project: string, id: string): MediaSource {
  const [src, setSrc] = useState<string | undefined>();
  const [error, setError] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const history = useRef<number[]>([]);
  const alive = useRef(true);
  const loadRef = useRef<(initial: boolean) => Promise<boolean>>(() => Promise.resolve(false));

  const load = useCallback(
    async (initial: boolean): Promise<boolean> => {
      clearTimeout(timer.current);
      const now = Date.now();
      if (!initial) {
        if (!refreshAllowed(history.current, now)) return false;
        history.current = [...history.current.filter((t) => now - t < 15_000), now];
      }
      try {
        const m = await requestMediaUrl(project, id);
        if (!alive.current) return false;
        setSrc(m.url);
        setError(null);
        timer.current = setTimeout(() => void loadRef.current(false), refreshDelay(m.expiresAt, Date.now()));
        return true;
      } catch (e) {
        if (!alive.current) return false;
        if (initial) setError(errorMessage(e));
        else timer.current = setTimeout(() => void loadRef.current(false), RETRY_MS);
        return false;
      }
    },
    [project, id],
  );

  useEffect(() => {
    loadRef.current = load;
  }, [load]);

  useEffect(() => {
    alive.current = true;
    history.current = [];
    setSrc(undefined);
    setError(null);
    void load(true);
    return () => {
      alive.current = false;
      clearTimeout(timer.current);
    };
  }, [load]);

  const refresh = useCallback(() => load(false), [load]);
  return { src, error, refresh };
}
