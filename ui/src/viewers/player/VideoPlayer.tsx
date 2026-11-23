import { ActionIcon, Alert, Button, Group, Menu, Text } from '@mantine/core';
import {
  IconArrowsMaximize,
  IconArrowsMinimize,
  IconCheck,
  IconDownload,
  IconPictureInPicture,
  IconPlayerPause,
  IconPlayerPlay,
  IconVolume,
  IconVolume3,
} from '@tabler/icons-react';
import { useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent, type Ref } from 'react';
import { attachmentUrl } from '../../api/client';
import type { Attachment } from '../../api/types';
import { usesToken } from '../../lib/authToken';
import { Loading } from '../../components/QueryState';
import { downloadWithToken } from '../authedUrl';
import { useMediaSource } from './mediaUrl';
import {
  FRAME_SECONDS,
  SPEEDS,
  boundsOf,
  browserStorage,
  classifyError,
  clamp,
  formatClock,
  fractionOf,
  keyAction,
  loadPrefs,
  nextSpeed,
  rangesOf,
  savePrefs,
  seekTarget,
  spokenClock,
  thumbSecond,
  timeAtFraction,
  type Bounds,
  type Marker,
  type PlaybackProblem,
  type PlayerAction,
  type Prefs,
  type Range,
} from './playerMath';
import { ThumbnailSource, canGenerateThumbnails } from './thumbs';
import './player.css';

export interface PlayerHandle {
  seekTo: (seconds: number) => void;
}

interface View {
  time: number;
  paused: boolean;
  ended: boolean;
  waiting: boolean;
  bounds: Bounds;
  buffered: Range[];
}

const INITIAL: View = { time: 0, paused: true, ended: false, waiting: false, bounds: { length: null, max: 0, known: false }, buffered: [] };

interface Hover {
  x: number;
  width: number;
  time: number;
}

export interface VideoPlayerProps {
  project: string;
  attachment: Pick<Attachment, 'id' | 'name' | 'size'>;
  markers?: readonly Marker[];
  failureAt?: number | null;
  onTime?: (seconds: number) => void;
  handleRef?: Ref<PlayerHandle>;
}

function DownloadControl({ project, attachment, label }: { project: string; attachment: Pick<Attachment, 'id' | 'name'>; label: string }) {
  const url = attachmentUrl(project, attachment.id, true);
  if (usesToken()) {
    return (
      <ActionIcon
        variant="subtle"
        color="gray"
        aria-label={label}
        title={label}
        onClick={() => void downloadWithToken(url, attachment.name).catch(() => undefined)}
      >
        <IconDownload size={18} />
      </ActionIcon>
    );
  }
  return (
    <ActionIcon component="a" href={url} download={attachment.name} variant="subtle" color="gray" aria-label={label} title={label}>
      <IconDownload size={18} />
    </ActionIcon>
  );
}

function ProblemBox({ problem, project, attachment, onRetry }: { problem: PlaybackProblem; project: string; attachment: Pick<Attachment, 'id' | 'name'>; onRetry: () => void }) {
  const url = attachmentUrl(project, attachment.id, true);
  const codec = problem === 'unsupported';
  return (
    <Alert color="red" variant="light" title={codec ? 'This browser cannot play this video' : 'The video could not be loaded'} data-testid="video-problem">
      <Group gap="sm" wrap="wrap">
        <Text size="sm" style={{ flex: '1 1 220px' }}>
          {codec ? 'The file uses a format or codec this browser does not support. Download it to watch it in another player.' : 'The connection failed or the address expired.'}
        </Text>
        {!codec && (
          <Button size="xs" variant="default" onClick={onRetry}>
            Try again
          </Button>
        )}
        {usesToken() ? (
          <Button size="xs" variant="default" leftSection={<IconDownload size={14} />} onClick={() => void downloadWithToken(url, attachment.name).catch(() => undefined)}>
            Download
          </Button>
        ) : (
          <Button component="a" href={url} download={attachment.name} size="xs" variant="default" leftSection={<IconDownload size={14} />} data-testid="video-problem-download">
            Download
          </Button>
        )}
      </Group>
    </Alert>
  );
}

export function VideoPlayer({ project, attachment, markers = [], failureAt = null, onTime, handleRef }: VideoPlayerProps) {
  const { src, error: sourceError, refresh } = useMediaSource(project, attachment.id);
  const rootRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const seekRef = useRef<HTMLDivElement>(null);
  const [view, setView] = useState<View>(INITIAL);
  const [prefs, setPrefs] = useState<Prefs>(() => loadPrefs(browserStorage()));
  const [problem, setProblem] = useState<PlaybackProblem | null>(null);
  const [hover, setHover] = useState<Hover | null>(null);
  const [live, setLive] = useState('');
  const [fullscreen, setFullscreen] = useState(false);
  const dragging = useRef(false);
  const last = useRef({ time: 0, playing: false });
  const resume = useRef<{ time: number; playing: boolean } | null>(null);
  const prevSrc = useRef<string | undefined>(undefined);
  const thumbs = useRef<ThumbnailSource | null>(null);
  const hoverSecond = useRef(-1);
  const [thumbSrc, setThumbSrc] = useState<string | undefined>();
  const announceTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const onTimeRef = useRef(onTime);
  const raf = useRef(0);

  useEffect(() => {
    onTimeRef.current = onTime;
  }, [onTime]);

  const sync = useCallback(() => {
    const v = videoRef.current;
    if (!v) return;
    const bounds = boundsOf(v);
    const time = v.currentTime;
    if (!v.error && v.readyState > 0) last.current = { time, playing: !v.paused && !v.ended };
    setView((prev) => {
      const buffered = rangesOf(v.buffered);
      const same =
        prev.paused === v.paused &&
        prev.ended === v.ended &&
        prev.bounds.max === bounds.max &&
        prev.bounds.known === bounds.known &&
        Math.abs(prev.time - time) < 0.02 &&
        prev.buffered.length === buffered.length &&
        prev.buffered.every((r, i) => {
          const b = buffered[i];
          return !!b && r.start === b.start && r.end === b.end;
        });
      if (same) return prev;
      return { time, paused: v.paused, ended: v.ended, waiting: prev.waiting, bounds, buffered };
    });
    onTimeRef.current?.(time);
  }, []);

  useEffect(() => {
    if (view.paused) return;
    const tick = () => {
      sync();
      raf.current = requestAnimationFrame(tick);
    };
    raf.current = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf.current);
  }, [view.paused, sync]);

  useEffect(() => {
    if (prevSrc.current && src && prevSrc.current !== src) resume.current = { ...last.current };
    prevSrc.current = src;
    if (src) thumbs.current?.setSrc(src);
  }, [src]);

  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    v.defaultPlaybackRate = prefs.speed;
    v.playbackRate = prefs.speed;
    v.volume = prefs.volume;
    v.muted = prefs.muted;
    savePrefs(browserStorage(), prefs);
  }, [prefs, src]);

  useEffect(() => {
    const onChange = () => setFullscreen(document.fullscreenElement === rootRef.current);
    document.addEventListener('fullscreenchange', onChange);
    return () => document.removeEventListener('fullscreenchange', onChange);
  }, []);

  useEffect(
    () => () => {
      thumbs.current?.dispose();
      thumbs.current = null;
      clearTimeout(announceTimer.current);
    },
    [],
  );

  const announce = useCallback((text: string) => {
    clearTimeout(announceTimer.current);
    announceTimer.current = setTimeout(() => setLive(text), 350);
  }, []);

  const seekTo = useCallback(
    (seconds: number, speak = true) => {
      const v = videoRef.current;
      if (!v) return;
      const bounds = boundsOf(v);
      const to = clamp(seconds, 0, bounds.max);
      v.currentTime = to;
      sync();
      if (speak) announce(`Position ${spokenClock(to)}${bounds.length !== null ? ` of ${spokenClock(bounds.length)}` : ''}`);
    },
    [announce, sync],
  );

  useImperativeHandle(handleRef, () => ({ seekTo: (s: number) => seekTo(s) }), [seekTo]);

  const toggle = useCallback(() => {
    const v = videoRef.current;
    if (!v) return;
    if (v.paused || v.ended) {
      if (v.ended) v.currentTime = 0;
      void v.play().catch(() => undefined);
    } else {
      v.pause();
    }
  }, []);

  const toggleFullscreen = useCallback(() => {
    const root = rootRef.current;
    if (!root) return;
    if (document.fullscreenElement) void document.exitFullscreen().catch(() => undefined);
    else void root.requestFullscreen().catch(() => undefined);
  }, []);

  const pip = typeof document !== 'undefined' && document.pictureInPictureEnabled;
  const togglePip = useCallback(() => {
    const v = videoRef.current;
    if (!v) return;
    if (document.pictureInPictureElement) void document.exitPictureInPicture().catch(() => undefined);
    else void v.requestPictureInPicture().catch(() => undefined);
  }, []);

  const run = useCallback(
    (a: PlayerAction) => {
      const v = videoRef.current;
      if (!v) return;
      const bounds = boundsOf(v);
      switch (a.type) {
        case 'toggle':
          toggle();
          break;
        case 'seekBy':
          seekTo(seekTarget(v.currentTime, a.seconds, bounds));
          break;
        case 'frame':
          seekTo(seekTarget(v.currentTime, a.direction * FRAME_SECONDS, bounds));
          break;
        case 'seekStart':
          seekTo(0);
          break;
        case 'seekEnd':
          seekTo(bounds.max);
          break;
        case 'fullscreen':
          toggleFullscreen();
          break;
        case 'mute':
          setPrefs((p) => ({ ...p, muted: !p.muted }));
          break;
        case 'speed':
          setPrefs((p) => ({ ...p, speed: nextSpeed(p.speed, a.direction) }));
          break;
      }
    },
    [seekTo, toggle, toggleFullscreen],
  );

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement;
    if (target.closest('[role="menu"], input, select, textarea')) return;
    if (target.closest('button, a') && (e.key === ' ' || e.key === 'Enter')) return;
    const action = keyAction(e, videoRef.current?.paused ?? true);
    if (!action) return;
    e.preventDefault();
    run(action);
  };

  const onError = useCallback(async () => {
    const v = videoRef.current;
    const current = v?.currentSrc || src;
    if (!v?.error || !current) return;
    const code = v.error.code;
    if (code === 1) return;
    const status = await fetch(current, { headers: { Range: 'bytes=0-0' }, cache: 'no-store' }).then(
      (res) => {
        void res.body?.cancel();
        return res.status;
      },
      () => null,
    );
    const kind = classifyError(code, status);
    if (kind === 'expired') {
      const ok = await refresh();
      if (!ok) setProblem('network');
    } else {
      setProblem(kind);
    }
  }, [refresh, src]);

  const onLoadedMetadata = () => {
    setProblem(null);
    const v = videoRef.current;
    const r = resume.current;
    if (v && r) {
      resume.current = null;
      v.currentTime = r.time;
      if (r.playing) void v.play().catch(() => undefined);
    }
    sync();
  };

  const fractionAt = (clientX: number): number => {
    const el = seekRef.current;
    if (!el) return 0;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 ? clamp((clientX - rect.left) / rect.width, 0, 1) : 0;
  };

  const onSeekPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0 || view.bounds.max <= 0) return;
    dragging.current = true;
    e.currentTarget.setPointerCapture(e.pointerId);
    seekTo(timeAtFraction(fractionAt(e.clientX), view.bounds), false);
    rootRef.current?.focus({ preventScroll: true });
  };

  const onSeekPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const f = fractionAt(e.clientX);
    const time = timeAtFraction(f, view.bounds);
    const el = seekRef.current;
    if (dragging.current) seekTo(time, false);
    if (el && e.pointerType !== 'touch') {
      setHover({ x: f * el.clientWidth, width: el.clientWidth, time });
      if (src && !dragging.current && canGenerateThumbnails()) {
        thumbs.current ??= new ThumbnailSource(src, (second) => {
          if (second === hoverSecond.current) setThumbSrc(thumbs.current?.get(second));
        });
        if (thumbs.current.usable) {
          const second = thumbSecond(time, view.bounds);
          hoverSecond.current = second;
          setThumbSrc(thumbs.current.get(second) ?? thumbs.current.nearest(second));
          thumbs.current.request(second);
        }
      }
    }
  };

  const onSeekPointerUp = (e: PointerEvent<HTMLDivElement>) => {
    if (!dragging.current) return;
    dragging.current = false;
    if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
    seekTo(timeAtFraction(fractionAt(e.clientX), view.bounds));
  };

  const { bounds } = view;
  const known = bounds.known;
  const fraction = fractionOf(view.time, bounds);
  const visibleMarkers = useMemo(() => markers.filter((m) => bounds.max > 0 && m.time <= bounds.max + 0.001), [markers, bounds.max]);
  const hoverMarker = useMemo(() => {
    if (!hover || bounds.max <= 0) return null;
    const reach = (7 / Math.max(1, hover.width)) * bounds.max;
    let best: Marker | null = null;
    for (const m of visibleMarkers) if (Math.abs(m.time - hover.time) <= reach && (!best || Math.abs(m.time - hover.time) < Math.abs(best.time - hover.time))) best = m;
    return best;
  }, [hover, visibleMarkers, bounds.max]);
  const timeText = known ? `${formatClock(view.time)} / ${formatClock(bounds.length)}` : formatClock(view.time);
  const valueText = known ? `${spokenClock(view.time)} of ${spokenClock(bounds.length ?? 0)}` : `${spokenClock(view.time)}, length not known`;
  const speedLabel = `${String(prefs.speed)}×`;

  if (sourceError) return <ProblemBox problem="network" project={project} attachment={attachment} onRetry={() => void refresh()} />;
  if (!src) return <Loading />;

  const showMarkerLabel = hoverMarker && hover;
  const tipLeft = hover ? clamp(hover.x, 90, Math.max(90, hover.width - 90)) : 0;

  return (
    <div
      ref={rootRef}
      className="fp-vp"
      data-fullscreen={fullscreen ? 'true' : undefined}
      tabIndex={0}
      role="group"
      aria-roledescription="video player"
      aria-label={`Video player: ${attachment.name}`}
      onKeyDown={onKeyDown}
      data-testid="video-player"
    >
      <div className="fp-vp-stage">
        <video
          ref={videoRef}
          src={src}
          preload="metadata"
          playsInline
          className="fp-vp-video"
          data-testid="video-viewer"
          onClick={toggle}
          onDoubleClick={toggleFullscreen}
          onLoadedMetadata={onLoadedMetadata}
          onDurationChange={sync}
          onTimeUpdate={sync}
          onProgress={sync}
          onPlay={sync}
          onPause={sync}
          onSeeking={sync}
          onSeeked={sync}
          onEnded={sync}
          onVolumeChange={sync}
          onWaiting={() => setView((p) => ({ ...p, waiting: true }))}
          onPlaying={() => setView((p) => ({ ...p, waiting: false }))}
          onCanPlay={() => setView((p) => ({ ...p, waiting: false }))}
          onError={() => void onError()}
        >
          <track kind="captions" />
        </video>
        {view.paused && !problem && (
          <button type="button" className="fp-vp-bigplay" tabIndex={-1} aria-hidden onClick={toggle}>
            <IconPlayerPlay size={34} />
          </button>
        )}
        {view.waiting && !view.paused && <div className="fp-vp-spinner" aria-hidden />}
      </div>
      {problem && <ProblemBox problem={problem} project={project} attachment={attachment} onRetry={() => void refresh().then(() => setProblem(null))} />}
      <div className="fp-vp-seekrow">
        <div
          ref={seekRef}
          className="fp-vp-seek"
          role="slider"
          tabIndex={0}
          aria-label="Seek"
          aria-valuemin={0}
          aria-valuemax={Math.max(0, Math.floor(bounds.max))}
          aria-valuenow={Math.floor(clamp(view.time, 0, Math.max(bounds.max, 0)))}
          aria-valuetext={valueText}
          data-testid="video-seek"
          onPointerDown={onSeekPointerDown}
          onPointerMove={onSeekPointerMove}
          onPointerUp={onSeekPointerUp}
          onPointerCancel={onSeekPointerUp}
          onPointerLeave={() => !dragging.current && setHover(null)}
        >
          <div className="fp-vp-track">
            {view.buffered.map((r) => (
              <div key={`${String(r.start)}-${String(r.end)}`} className="fp-vp-buffered" style={{ left: `${String(fractionOf(r.start, bounds) * 100)}%`, width: `${String((fractionOf(r.end, bounds) - fractionOf(r.start, bounds)) * 100)}%` }} />
            ))}
            <div className="fp-vp-played" style={{ width: `${String(fraction * 100)}%` }} />
          </div>
          {visibleMarkers.map((m) => (
            <span
              key={m.key}
              className={`fp-vp-marker${m.depth > 0 ? ' fp-vp-marker-nested' : ''}${m.failed ? ' fp-vp-marker-failed' : ''}`}
              style={{ left: `${String(fractionOf(m.time, bounds) * 100)}%` }}
              data-testid="video-marker"
              data-depth={m.depth}
              data-failed={m.failed ? 'true' : undefined}
              aria-hidden
            />
          ))}
          {failureAt !== null && bounds.max > 0 && failureAt <= bounds.max + 0.001 && (
            <span className="fp-vp-failure" style={{ left: `${String(fractionOf(failureAt, bounds) * 100)}%` }} data-testid="video-failure" title={`Failure at ${formatClock(failureAt)}`} aria-hidden />
          )}
          <div className="fp-vp-thumb" style={{ left: `${String(fraction * 100)}%` }} aria-hidden />
          {hover && (
            <div className="fp-vp-tip" style={{ left: tipLeft }} aria-hidden data-testid="video-hover-tip">
              {thumbSrc && <img src={thumbSrc} alt="" className="fp-vp-tipimg" />}
              {showMarkerLabel && <div className="fp-vp-tipname">{hoverMarker.name}</div>}
              <div className="fp-vp-tiptime">{formatClock(hover.time)}</div>
            </div>
          )}
        </div>
      </div>
      <div className="fp-vp-controls">
        <ActionIcon variant="subtle" color="gray" onClick={toggle} aria-label={view.paused || view.ended ? 'Play' : 'Pause'} title={view.paused || view.ended ? 'Play (k)' : 'Pause (k)'} data-testid="video-play">
          {view.paused || view.ended ? <IconPlayerPlay size={20} /> : <IconPlayerPause size={20} />}
        </ActionIcon>
        <Text size="sm" className="fp-vp-time" data-testid="video-time" aria-hidden>
          {timeText}
        </Text>
        <div className="fp-vp-spacer" />
        <Menu position="top-end" withinPortal>
          <Menu.Target>
            <Button variant="subtle" color="gray" size="compact-sm" aria-label={`Playback speed, ${speedLabel}`} title="Playback speed" data-testid="video-speed">
              {speedLabel}
            </Button>
          </Menu.Target>
          <Menu.Dropdown>
            {SPEEDS.map((s) => (
              <Menu.Item key={s} onClick={() => setPrefs((p) => ({ ...p, speed: s }))} rightSection={s === prefs.speed ? <IconCheck size={14} /> : undefined} aria-checked={s === prefs.speed} role="menuitemradio">
                {s}×
              </Menu.Item>
            ))}
          </Menu.Dropdown>
        </Menu>
        <ActionIcon variant="subtle" color="gray" onClick={() => setPrefs((p) => ({ ...p, muted: !p.muted }))} aria-label={prefs.muted || prefs.volume === 0 ? 'Unmute' : 'Mute'} title="Mute (m)">
          {prefs.muted || prefs.volume === 0 ? <IconVolume3 size={18} /> : <IconVolume size={18} />}
        </ActionIcon>
        <input
          type="range"
          className="fp-vp-volume"
          min={0}
          max={1}
          step={0.05}
          value={prefs.muted ? 0 : prefs.volume}
          aria-label="Volume"
          onChange={(e) => setPrefs((p) => ({ ...p, volume: Number(e.target.value), muted: Number(e.target.value) === 0 }))}
        />
        {pip && (
          <ActionIcon variant="subtle" color="gray" onClick={togglePip} aria-label="Picture in picture" title="Picture in picture">
            <IconPictureInPicture size={18} />
          </ActionIcon>
        )}
        <DownloadControl project={project} attachment={attachment} label="Download the video" />
        <ActionIcon variant="subtle" color="gray" onClick={toggleFullscreen} aria-label={fullscreen ? 'Exit full screen' : 'Full screen'} title="Full screen (f)">
          {fullscreen ? <IconArrowsMinimize size={18} /> : <IconArrowsMaximize size={18} />}
        </ActionIcon>
      </div>
      <div role="status" aria-live="polite" className="fp-sr-only" data-testid="video-live">
        {live}
      </div>
      {!known && view.bounds.max > 0 && (
        <Text size="xs" c="dimmed" className="fp-vp-note">
          The length of this recording is not stored in the file, so only the time played is shown.
        </Text>
      )}
    </div>
  );
}

