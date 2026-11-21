import { useQueryClient, type QueryClient } from '@tanstack/react-query';
import { useEffect, useState } from 'react';
import { projectPath } from './client';
import { eventStreamSupported, openEventStream } from './sse';
import { qk } from './hooks';
import type { LaunchUpdatedEvent, UploadUpdatedEvent } from './types';

export type LiveState = 'connecting' | 'live' | 'offline' | 'unsupported';

function parse(data: unknown): unknown {
  if (typeof data !== 'string') return null;
  try {
    return JSON.parse(data) as unknown;
  } catch {
    return null;
  }
}

export function onLaunchUpdated(qc: QueryClient, key: string, ev: LaunchUpdatedEvent): void {
  void qc.invalidateQueries({ queryKey: qk.launch(key, ev.id) });
  void qc.invalidateQueries({ queryKey: qk.launches(key) });
  void qc.invalidateQueries({ queryKey: qk.results(key, ev.id) });
  void qc.invalidateQueries({ queryKey: qk.tree(key, ev.id) });
  void qc.invalidateQueries({ queryKey: qk.projects() });
}

export function onUploadUpdated(qc: QueryClient, key: string, ev: UploadUpdatedEvent): void {
  void qc.invalidateQueries({ queryKey: qk.uploads(key, ev.launchId) });
  void qc.invalidateQueries({ queryKey: qk.launch(key, ev.launchId) });
}

export function useProjectEvents(key: string | undefined): LiveState {
  const qc = useQueryClient();
  const [state, setState] = useState<LiveState>(eventStreamSupported() ? 'connecting' : 'unsupported');

  useEffect(() => {
    if (!key || !eventStreamSupported()) return;
    const pending = new Map<string, ReturnType<typeof setTimeout>>();
    const debounce = (id: string, fn: () => void) => {
      if (pending.has(id)) return;
      pending.set(
        id,
        setTimeout(() => {
          pending.delete(id);
          fn();
        }, 400),
      );
    };
    setState('connecting');
    const es = openEventStream(`${projectPath(key)}/events`, {
      onOpen: () => setState('live'),
      onError: (closed) => setState(closed ? 'offline' : 'connecting'),
      events: {
        'launch.updated': (data) => {
          const ev = parse(data) as LaunchUpdatedEvent | null;
          if (ev?.id) debounce(`l:${ev.id}`, () => onLaunchUpdated(qc, key, ev));
        },
        'upload.updated': (data) => {
          const ev = parse(data) as UploadUpdatedEvent | null;
          if (ev?.launchId) debounce(`u:${ev.launchId}`, () => onUploadUpdated(qc, key, ev));
        },
      },
    });
    return () => {
      es.close();
      for (const t of pending.values()) clearTimeout(t);
      pending.clear();
    };
  }, [key, qc]);

  return state;
}
