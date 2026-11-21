import { Button, Center, Loader } from '@mantine/core';
import { useEffect, useRef } from 'react';

export function LoadMore({ hasMore, loading, onLoad, label = 'Load more' }: { hasMore: boolean; loading: boolean; onLoad: () => void; label?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el || !hasMore || typeof IntersectionObserver === 'undefined') return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting) && !loading) onLoad();
      },
      { rootMargin: '400px' },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [hasMore, loading, onLoad]);
  if (!hasMore) return null;
  return (
    <>
      <div ref={ref} className="fp-sentinel" data-testid="load-more-sentinel" />
      <Center py="sm">
        {loading ? (
          <Loader size="sm" />
        ) : (
          <Button variant="default" size="xs" onClick={onLoad}>
            {label}
          </Button>
        )}
      </Center>
    </>
  );
}
