import { Group, SegmentedControl, SimpleGrid, Slider, Stack, Text } from '@mantine/core';
import { useCallback, useRef, useState, type PointerEvent } from 'react';

export interface CompareImages {
  expected: string;
  actual: string;
  diff?: string;
}

type Mode = 'slider' | 'sideBySide' | 'diff' | 'actual' | 'expected';

export function ImageCompare({ images, name }: { images: CompareImages; name: string }) {
  const [mode, setMode] = useState<Mode>('slider');
  const [pos, setPos] = useState(50);
  const box = useRef<HTMLDivElement>(null);
  const dragging = useRef(false);

  const move = useCallback((clientX: number) => {
    const el = box.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    if (r.width <= 0) return;
    setPos(Math.max(0, Math.min(100, ((clientX - r.left) / r.width) * 100)));
  }, []);

  const onDown = (e: PointerEvent<HTMLDivElement>) => {
    dragging.current = true;
    e.currentTarget.setPointerCapture(e.pointerId);
    move(e.clientX);
  };
  const onMove = (e: PointerEvent<HTMLDivElement>) => {
    if (dragging.current) move(e.clientX);
  };
  const onUp = () => {
    dragging.current = false;
  };

  const modes = [
    { value: 'slider', label: 'Slider' },
    { value: 'sideBySide', label: 'Side by side' },
    ...(images.diff ? [{ value: 'diff', label: 'Diff' }] : []),
    { value: 'expected', label: 'Expected' },
    { value: 'actual', label: 'Actual' },
  ];

  return (
    <Stack gap="sm" data-testid="image-compare">
      <SegmentedControl size="xs" value={mode} onChange={(v) => setMode(v as Mode)} data={modes} style={{ alignSelf: 'flex-start', maxWidth: '100%', overflowX: 'auto' }} />
      {mode === 'slider' && (
        <Stack gap={6}>
          <Group justify="space-between">
            <Text size="xs" c="dimmed">
              Expected
            </Text>
            <Text size="xs" c="dimmed">
              Actual
            </Text>
          </Group>
          <div ref={box} className="fp-compare" onPointerDown={onDown} onPointerMove={onMove} onPointerUp={onUp} onPointerCancel={onUp} data-testid="compare-slider-box">
            <img src={images.actual} alt={`${name}: actual`} draggable={false} />
            <div className="fp-compare-top" style={{ clipPath: `inset(0 ${String(100 - pos)}% 0 0)` }}>
              <img src={images.expected} alt={`${name}: expected`} draggable={false} />
            </div>
            <div className="fp-compare-handle" style={{ left: `calc(${String(pos)}% - 1px)` }} />
          </div>
          <Slider value={pos} onChange={setPos} label={(v) => `${String(Math.round(v))}%`} aria-label="Expected and actual split" size="sm" />
        </Stack>
      )}
      {mode === 'sideBySide' && (
        <SimpleGrid cols={{ base: 1, sm: images.diff ? 3 : 2 }} spacing="sm">
          {(
            [
              ['Expected', images.expected],
              ['Actual', images.actual],
              ...(images.diff ? [['Diff', images.diff] as const] : []),
            ] as const
          ).map(([label, src]) => (
            <Stack gap={4} key={label}>
              <Text size="xs" c="dimmed">
                {label}
              </Text>
              <div className="fp-compare">
                <img src={src} alt={`${name}: ${label.toLowerCase()}`} />
              </div>
            </Stack>
          ))}
        </SimpleGrid>
      )}
      {(mode === 'diff' || mode === 'actual' || mode === 'expected') && (
        <div className="fp-compare">
          <img src={mode === 'diff' ? images.diff : mode === 'actual' ? images.actual : images.expected} alt={`${name}: ${mode}`} />
        </div>
      )}
    </Stack>
  );
}
