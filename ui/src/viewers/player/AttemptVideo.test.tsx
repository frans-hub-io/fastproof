import { act, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { HttpResponse, http } from 'msw';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { useLocation } from 'react-router';
import type { Attempt, Step } from '../../api/types';
import { server } from '../../mocks/node';
import { renderWith } from '../../test/render';
import { AttemptVideo } from './AttemptVideo';

const START = '2026-11-22T10:00:00.000Z';
const base = Date.parse(START);

function step(name: string, at: number | null, ms: number, status = 'passed', steps: Step[] = []): Step {
  return {
    name,
    status: status as Step['status'],
    kind: 'step',
    startedAt: at === null ? null : new Date(base + at).toISOString(),
    stoppedAt: at === null ? null : new Date(base + at + ms).toISOString(),
    durationMs: ms,
    error: null,
    parameters: [],
    attachments: [],
    steps,
  };
}

const steps: Step[] = [step('open', 1000, 2000), step('pay', 10_000, 20_000, 'failed', [step('fill', 12_000, 3000), step('undated', null, 5)]), step('close', 40_000, 500)];

function attempt(videos = 1): Pick<Attempt, 'startedAt' | 'steps' | 'attachments'> {
  return {
    startedAt: START,
    steps,
    attachments: Array.from({ length: videos }, (_, i) => ({ id: `v${String(i + 1)}`, name: `video-${String(i + 1)}.webm`, contentType: 'video/webm', kind: 'video', size: 1000 })),
  };
}

function Where() {
  const l = useLocation();
  return <output data-testid="where">{l.search}</output>;
}

const proto = HTMLMediaElement.prototype;
const original = {
  duration: Object.getOwnPropertyDescriptor(proto, 'duration'),
  seekable: Object.getOwnPropertyDescriptor(proto, 'seekable'),
};

describe('video with steps', () => {
  beforeEach(() => {
    Object.defineProperty(proto, 'duration', { configurable: true, get: () => 120 });
    Object.defineProperty(proto, 'seekable', { configurable: true, get: () => ({ length: 1, start: () => 0, end: () => 120 }) });
    server.use(
      http.post('/api/v1/projects/web-shop/attachments/:id/media-url', ({ params }) =>
        HttpResponse.json({ url: `/api/v1/media/${String(params.id)}`, expiresAt: new Date(Date.now() + 900_000).toISOString() }),
      ),
    );
  });
  afterEach(() => {
    if (original.duration) Object.defineProperty(proto, 'duration', original.duration);
    if (original.seekable) Object.defineProperty(proto, 'seekable', original.seekable);
  });

  it('puts the steps on the timeline and seeks when a step is clicked', async () => {
    const user = userEvent.setup();
    renderWith(<AttemptVideo project="web-shop" attempt={attempt()} />);
    const video = await screen.findByTestId<HTMLVideoElement>('video-viewer');
    expect(video).toHaveAttribute('src', '/api/v1/media/v1');
    fireEvent.loadedMetadata(video);
    await waitFor(() => expect(screen.getAllByTestId('video-marker')).toHaveLength(4));
    expect(screen.getAllByTestId('video-marker').filter((m) => m.dataset.depth === '1')).toHaveLength(1);
    expect(screen.getByTestId('video-failure')).toBeInTheDocument();
    expect(screen.getByTestId('video-approximate')).toHaveTextContent('approximate');

    const rows = screen.getAllByTestId('video-step');
    expect(rows).toHaveLength(5);
    expect(rows[3]).toBeDisabled();
    await user.click(within(rows[1] as HTMLElement).getByText('pay'));
    expect(video.currentTime).toBeCloseTo(10);
    expect(screen.getByTestId('video-live')).toBeInTheDocument();
  });

  it('highlights the step that is playing', async () => {
    renderWith(<AttemptVideo project="web-shop" attempt={attempt()} />);
    const video = await screen.findByTestId<HTMLVideoElement>('video-viewer');
    fireEvent.loadedMetadata(video);
    act(() => {
      video.currentTime = 13;
      fireEvent.timeUpdate(video);
    });
    await waitFor(() => expect(screen.getAllByTestId('video-step')[2]).toHaveAttribute('aria-current', 'step'));
    act(() => {
      video.currentTime = 41;
      fireEvent.timeUpdate(video);
    });
    await waitFor(() => expect(screen.getAllByTestId('video-step')[4]).toHaveAttribute('aria-current', 'step'));
    expect(screen.getAllByTestId('video-step')[2]).not.toHaveAttribute('aria-current');
  });

  it('keeps the nudge in the address and moves the markers', async () => {
    const user = userEvent.setup();
    renderWith(
      <>
        <AttemptVideo project="web-shop" attempt={attempt()} />
        <Where />
      </>,
    );
    await screen.findByTestId('video-viewer');
    await user.click(screen.getByTestId('video-offset-plus'));
    await user.click(screen.getByTestId('video-offset-plus'));
    expect(screen.getByTestId('video-offset')).toHaveTextContent('+1.0 s');
    expect(screen.getByTestId('where')).toHaveTextContent('vo=v1%3A1000');
    await user.click(screen.getByTestId('video-offset-minus'));
    expect(screen.getByTestId('video-offset')).toHaveTextContent('+0.5 s');
    await user.click(screen.getByRole('button', { name: 'Reset' }));
    expect(screen.getByTestId('where')).toHaveTextContent(/^$/);
  });

  it('restores the nudge from the address', async () => {
    renderWith(<AttemptVideo project="web-shop" attempt={attempt()} />, '/?vo=v1:-1500');
    await screen.findByTestId('video-viewer');
    expect(screen.getByTestId('video-offset')).toHaveTextContent('-1.5 s');
    expect(screen.getAllByTestId('video-step')[0]).toHaveAccessibleName(/at 0:00/);
  });

  it('switches between the videos of one attempt', async () => {
    const user = userEvent.setup();
    renderWith(<AttemptVideo project="web-shop" attempt={attempt(2)} />);
    expect(await screen.findByTestId('video-viewer')).toHaveAttribute('src', '/api/v1/media/v1');
    await user.click(screen.getByText('2. video-2.webm'));
    await waitFor(() => expect(screen.getByTestId('video-viewer')).toHaveAttribute('src', '/api/v1/media/v2'));
  });

  it('shows no switcher for one video, and nothing without a video', async () => {
    const { unmount } = renderWith(<AttemptVideo project="web-shop" attempt={attempt()} />);
    await screen.findByTestId('video-viewer');
    expect(screen.queryByTestId('video-switcher')).toBeNull();
    unmount();
    renderWith(<AttemptVideo project="web-shop" attempt={{ ...attempt(), attachments: [] }} />);
    expect(screen.queryByTestId('attempt-video')).toBeNull();
  });

  it('puts steps without a start in the list but not on the timeline', async () => {
    renderWith(<AttemptVideo project="web-shop" attempt={{ ...attempt(), steps: [step('undated', null, 100)] }} />);
    await screen.findByTestId('video-viewer');
    expect(screen.queryAllByTestId('video-marker')).toHaveLength(0);
    expect(screen.getByTestId('video-step')).toBeDisabled();
  });
});

describe('player controls', () => {
  beforeEach(() => {
    Object.defineProperty(proto, 'duration', { configurable: true, get: () => 120 });
    Object.defineProperty(proto, 'seekable', { configurable: true, get: () => ({ length: 1, start: () => 0, end: () => 120 }) });
    server.use(http.post('/api/v1/projects/web-shop/attachments/:id/media-url', () => HttpResponse.json({ url: '/api/v1/media/x', expiresAt: new Date(Date.now() + 900_000).toISOString() })));
  });
  afterEach(() => {
    if (original.duration) Object.defineProperty(proto, 'duration', original.duration);
    if (original.seekable) Object.defineProperty(proto, 'seekable', original.seekable);
  });

  it('seeks with the arrow keys, Shift and Home/End only while the player has focus', async () => {
    renderWith(<AttemptVideo project="web-shop" attempt={attempt()} />);
    const video = await screen.findByTestId<HTMLVideoElement>('video-viewer');
    fireEvent.loadedMetadata(video);
    const player = screen.getByTestId('video-player');
    fireEvent.keyDown(document.body, { key: 'ArrowRight' });
    expect(video.currentTime).toBe(0);
    fireEvent.keyDown(player, { key: 'ArrowRight' });
    expect(video.currentTime).toBe(5);
    fireEvent.keyDown(player, { key: 'ArrowRight', shiftKey: true });
    expect(video.currentTime).toBe(65);
    fireEvent.keyDown(player, { key: 'ArrowLeft' });
    expect(video.currentTime).toBe(60);
    fireEvent.keyDown(player, { key: 'End' });
    expect(video.currentTime).toBe(120);
    fireEvent.keyDown(player, { key: 'Home' });
    expect(video.currentTime).toBe(0);
  });

  it('labels the controls and exposes the seek bar as a slider', async () => {
    renderWith(<AttemptVideo project="web-shop" attempt={attempt()} />);
    const video = await screen.findByTestId('video-viewer');
    fireEvent.loadedMetadata(video);
    expect(screen.getByRole('button', { name: 'Play' })).toBeInTheDocument();
    expect(screen.getByRole('slider', { name: 'Seek' })).toHaveAttribute('aria-valuemax', '120');
    expect(screen.getByRole('slider', { name: 'Volume' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Full screen' })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Download the video' })).toHaveAttribute('href', '/api/v1/projects/web-shop/attachments/v1/content?download=1');
    expect(screen.getByTestId('video-time')).toHaveTextContent('0:00 / 2:00');
  });

  it('shows elapsed time only when the length is not stored in the file', async () => {
    Object.defineProperty(proto, 'duration', { configurable: true, get: () => Infinity });
    Object.defineProperty(proto, 'seekable', { configurable: true, get: () => ({ length: 1, start: () => 0, end: () => 30 }) });
    renderWith(<AttemptVideo project="web-shop" attempt={attempt()} />);
    const video = await screen.findByTestId<HTMLVideoElement>('video-viewer');
    fireEvent.loadedMetadata(video);
    await waitFor(() => expect(screen.getByRole('slider', { name: 'Seek' })).toHaveAttribute('aria-valuemax', '30'));
    expect(screen.getByTestId('video-time')).toHaveTextContent(/^0:00$/);
    fireEvent.keyDown(screen.getByTestId('video-player'), { key: 'End' });
    expect(video.currentTime).toBe(30);
  });

  it('explains an unsupported codec with a download link', async () => {
    server.use(http.get('/api/v1/media/x', () => new HttpResponse(new Uint8Array(1), { status: 206 })));
    renderWith(<AttemptVideo project="web-shop" attempt={attempt()} />);
    const video = await screen.findByTestId<HTMLVideoElement>('video-viewer');
    Object.defineProperty(video, 'error', { configurable: true, value: { code: 4 } });
    fireEvent.error(video);
    const box = await screen.findByTestId('video-problem');
    expect(box).toHaveTextContent('cannot play');
    expect(within(box).getByTestId('video-problem-download')).toHaveAttribute('href', expect.stringContaining('download=1'));
  });

  it('asks for a new address and resumes from the same place after a 404', async () => {
    let issued = 0;
    server.use(
      http.post('/api/v1/projects/web-shop/attachments/:id/media-url', () => {
        issued += 1;
        return HttpResponse.json({ url: `/api/v1/media/n${String(issued)}`, expiresAt: new Date(Date.now() + 900_000).toISOString() });
      }),
      http.get('/api/v1/media/n1', () => HttpResponse.json({ code: 'not_found', message: 'expired' }, { status: 404 })),
    );
    renderWith(<AttemptVideo project="web-shop" attempt={attempt()} />);
    const video = await screen.findByTestId<HTMLVideoElement>('video-viewer');
    expect(video).toHaveAttribute('src', '/api/v1/media/n1');
    Object.defineProperty(video, 'readyState', { configurable: true, value: 1 });
    fireEvent.loadedMetadata(video);
    act(() => {
      video.currentTime = 42;
      fireEvent.timeUpdate(video);
    });
    Object.defineProperty(video, 'error', { configurable: true, value: { code: 4 } });
    fireEvent.error(video);
    await waitFor(() => expect(screen.getByTestId('video-viewer')).toHaveAttribute('src', '/api/v1/media/n2'));
    expect(screen.queryByTestId('video-problem')).toBeNull();
    const next = screen.getByTestId<HTMLVideoElement>('video-viewer');
    Object.defineProperty(next, 'error', { configurable: true, value: null });
    fireEvent.loadedMetadata(next);
    expect(next.currentTime).toBe(42);
  });
});
