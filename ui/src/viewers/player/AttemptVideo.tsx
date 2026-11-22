import { ActionIcon, Badge, Button, Group, SegmentedControl, Stack, Text, Tooltip, UnstyledButton } from '@mantine/core';
import { IconMinus, IconPlus } from '@tabler/icons-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useSearchParams } from 'react-router';
import type { Attempt } from '../../api/types';
import { StatusDot } from '../../components/Status';
import { formatDuration } from '../../lib/format';
import {
  OFFSET_STEP_MS,
  buildMarkers,
  clampOffset,
  currentMarker,
  failureTime,
  flattenSteps,
  formatClock,
  formatOffset,
  parseOffsets,
  serializeOffsets,
  type Marker,
} from './playerMath';
import { videosOf } from './videos';
import { VideoPlayer, type PlayerHandle } from './VideoPlayer';

export const VIDEO_PARAM = 'video';
export const OFFSET_PARAM = 'vo';

const APPROXIMATE =
  'Playwright starts recording when the page opens, which is close to the start of the test but not exactly at it. Use the nudge buttons to line the steps up with the video.';

export function AttemptVideo({ project, attempt }: { project: string; attempt: Pick<Attempt, 'startedAt' | 'steps' | 'attachments'> }) {
  const videos = useMemo(() => videosOf(attempt.attachments), [attempt.attachments]);
  const [sp, setSp] = useSearchParams();
  const wanted = sp.get(VIDEO_PARAM);
  const video = videos.find((v) => v.id === wanted) ?? videos[0];
  const [offsets, setOffsets] = useState(() => parseOffsets(sp.get(OFFSET_PARAM)));
  const written = useRef(serializeOffsets(offsets));
  const offsetMs = video ? (offsets[video.id] ?? 0) : 0;
  const markers = useMemo(() => buildMarkers(attempt.startedAt, attempt.steps, offsetMs), [attempt.startedAt, attempt.steps, offsetMs]);
  const markerByKey = useMemo(() => new Map<string, Marker>(markers.map((m) => [m.key, m])), [markers]);
  const rows = useMemo(() => flattenSteps(attempt.steps), [attempt.steps]);
  const failureAt = useMemo(() => failureTime(markers), [markers]);
  const player = useRef<PlayerHandle>(null);
  const list = useRef<HTMLDivElement>(null);
  const markersRef = useRef(markers);
  const [currentKey, setCurrentKey] = useState<string | null>(null);

  useEffect(() => {
    markersRef.current = markers;
  }, [markers]);

  const update = useCallback(
    (fn: (p: URLSearchParams) => void) =>
      setSp(
        (prev) => {
          const n = new URLSearchParams(prev);
          fn(n);
          return n;
        },
        { replace: true },
      ),
    [setSp],
  );

  const setOffset = (change: (current: number) => number) => {
    if (!video) return;
    setOffsets((all) => ({ ...all, [video.id]: clampOffset(change(all[video.id] ?? 0)) }));
  };

  const onTime = useCallback((t: number) => {
    const k = currentMarker(markersRef.current, t)?.key ?? null;
    setCurrentKey((prev) => (prev === k ? prev : k));
  }, []);

  useEffect(() => {
    const box = list.current;
    if (!box || !currentKey) return;
    const el = box.querySelector<HTMLElement>(`[data-step-key="${currentKey}"]`);
    if (!el) return;
    const top = el.offsetTop;
    if (top < box.scrollTop) box.scrollTop = top - 4;
    else if (top + el.offsetHeight > box.scrollTop + box.clientHeight) box.scrollTop = top + el.offsetHeight - box.clientHeight + 4;
  }, [currentKey]);

  useEffect(() => {
    const value = serializeOffsets(offsets);
    if (value === written.current) return;
    written.current = value;
    update((n) => {
      if (value) n.set(OFFSET_PARAM, value);
      else n.delete(OFFSET_PARAM);
    });
  }, [offsets, update]);

  if (!video) return null;

  return (
    <Stack gap="sm" data-testid="attempt-video">
      {videos.length > 1 && (
        <SegmentedControl
          size="xs"
          value={video.id}
          onChange={(id) => update((n) => n.set(VIDEO_PARAM, id))}
          data={videos.map((v, i) => ({ value: v.id, label: `${String(i + 1)}. ${v.name}` }))}
          aria-label="Choose a video of this attempt"
          data-testid="video-switcher"
          style={{ maxWidth: '100%', overflowX: 'auto' }}
        />
      )}
      <div className="fp-vp-layout">
        <div style={{ minWidth: 0 }}>
          <VideoPlayer key={video.id} project={project} attachment={video} markers={markers} failureAt={failureAt} onTime={onTime} handleRef={player} />
        </div>
        <div className="fp-vp-steps" data-testid="video-steps">
          <Group gap="xs" wrap="wrap" justify="space-between" className="fp-vp-stepshead">
            <Group gap={6} wrap="nowrap">
              <Text size="sm" fw={600}>
                Steps
              </Text>
              <Tooltip label={APPROXIMATE} multiline w={260} events={{ hover: true, focus: true, touch: true }}>
                <Badge size="xs" variant="light" color="gray" radius="sm" tabIndex={0} style={{ textTransform: 'none', cursor: 'help' }} data-testid="video-approximate">
                  approximate
                </Badge>
              </Tooltip>
            </Group>
            <Group gap={4} wrap="nowrap" role="group" aria-label="Shift the steps against the video">
              <ActionIcon size="sm" variant="default" aria-label="Move the steps 0.5 seconds earlier" onClick={() => setOffset((c) => c - OFFSET_STEP_MS)} data-testid="video-offset-minus">
                <IconMinus size={14} />
              </ActionIcon>
              <Text size="xs" className="fp-vp-offset" data-testid="video-offset" aria-live="polite">
                {formatOffset(offsetMs)}
              </Text>
              <ActionIcon size="sm" variant="default" aria-label="Move the steps 0.5 seconds later" onClick={() => setOffset((c) => c + OFFSET_STEP_MS)} data-testid="video-offset-plus">
                <IconPlus size={14} />
              </ActionIcon>
              {offsetMs !== 0 && (
                <Button size="compact-xs" variant="subtle" color="gray" onClick={() => setOffset(() => 0)}>
                  Reset
                </Button>
              )}
            </Group>
          </Group>
          <div ref={list} className="fp-vp-steplist" role="list">
            {rows.length === 0 && (
              <Text size="sm" c="dimmed" p="xs">
                No steps were reported.
              </Text>
            )}
            {rows.map(({ key, depth, step }) => {
              const marker = markerByKey.get(key);
              const failing = step.status === 'failed' || step.status === 'broken';
              const current = key === currentKey;
              return (
                <div role="listitem" key={key}>
                  <UnstyledButton
                    className="fp-tree-row fp-vp-step"
                    data-step-key={key}
                    data-current={current ? 'true' : undefined}
                    data-status={step.status}
                    data-testid="video-step"
                    disabled={!marker}
                    aria-current={current ? 'step' : undefined}
                    aria-label={marker ? `${step.name}, at ${formatClock(marker.time)}. Jump to this moment.` : `${step.name}, no time recorded`}
                    onClick={() => marker && player.current?.seekTo(marker.time)}
                    style={{ paddingLeft: 6 + depth * 14 }}
                  >
                    <Group gap={6} wrap="nowrap">
                      <StatusDot status={step.status} />
                      <Text size="sm" fw={failing || current ? 600 : 400} style={{ flex: 1, minWidth: 0, wordBreak: 'break-word' }}>
                        {step.name}
                      </Text>
                      <Text size="xs" c="dimmed" className="fp-vp-steptime">
                        {marker ? formatClock(marker.time) : formatDuration(step.durationMs)}
                      </Text>
                    </Group>
                  </UnstyledButton>
                </div>
              );
            })}
          </div>
        </div>
      </div>
    </Stack>
  );
}
