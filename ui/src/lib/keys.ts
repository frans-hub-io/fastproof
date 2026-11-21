import { useEffect } from 'react';

export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || target.isContentEditable;
}

export interface ListKeysOptions {
  count: number;
  index: number;
  setIndex: (i: number) => void;
  onOpen?: (i: number) => void;
  enabled?: boolean;
}

export function useListKeys({ count, index, setIndex, onOpen, enabled = true }: ListKeysOptions): void {
  useEffect(() => {
    if (!enabled) return;
    const handler = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey || isTypingTarget(e.target)) return;
      if (document.querySelector('[role="dialog"]')) return;
      if (e.key === 'j') {
        e.preventDefault();
        setIndex(Math.min(count - 1, index + 1));
      } else if (e.key === 'k') {
        e.preventDefault();
        setIndex(Math.max(0, index - 1));
      } else if (e.key === 'Enter' && index >= 0 && index < count && onOpen) {
        if (e.target instanceof HTMLElement && (e.target.tagName === 'BUTTON' || e.target.tagName === 'A')) return;
        e.preventDefault();
        onOpen(index);
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [count, index, setIndex, onOpen, enabled]);

  useEffect(() => {
    if (index < 0) return;
    const el = document.querySelector<HTMLElement>(`[data-list-index="${String(index)}"]`);
    el?.scrollIntoView({ block: 'nearest' });
  }, [index]);
}
