import { apiFetch } from '@franshub/ui-shell';
import { useEffect, useState } from 'react';
import { usesToken } from '../lib/authToken';

/** Largest attachment fetched into memory for display while signed in with a token. */
export const MAX_INLINE_BYTES = 200 * 1024 * 1024;

async function fetchBlob(url: string): Promise<Blob> {
  const res = await apiFetch(url);
  if (!res.ok) throw new Error(`HTTP ${String(res.status)}`);
  const size = Number(res.headers.get('Content-Length') ?? '0');
  if (size > MAX_INLINE_BYTES) throw new Error('too large to show; download it instead');
  return res.blob();
}

/**
 * A URL that `<img>` and `<video>` can load. With a cookie session it is `url` itself; signed in
 * with an API token (which elements cannot send) the content is fetched with the token and shown
 * from a `blob:` URL, revoked when the component unmounts.
 */
export function useAuthedUrl(url: string | undefined): { src: string | undefined; error: string | null } {
  const token = usesToken();
  const [state, setState] = useState<{ src: string | undefined; error: string | null }>({ src: token ? undefined : url, error: null });
  useEffect(() => {
    if (!url || !token) {
      setState({ src: url, error: null });
      return;
    }
    let objectUrl: string | null = null;
    let alive = true;
    setState({ src: undefined, error: null });
    fetchBlob(url)
      .then((b) => {
        if (!alive) return;
        objectUrl = URL.createObjectURL(b);
        setState({ src: objectUrl, error: null });
      })
      .catch((e: unknown) => {
        if (alive) setState({ src: undefined, error: e instanceof Error ? e.message : String(e) });
      });
    return () => {
      alive = false;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [url, token]);
  return state;
}

/** Saves an attachment: a plain link with a cookie session, a fetch with the token otherwise. */
export async function downloadWithToken(url: string, name: string): Promise<void> {
  const res = await apiFetch(url);
  if (!res.ok) throw new Error(`HTTP ${String(res.status)}`);
  const objectUrl = URL.createObjectURL(await res.blob());
  const a = document.createElement('a');
  a.href = objectUrl;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(objectUrl), 10_000);
}
