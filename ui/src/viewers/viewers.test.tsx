import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';
import type { Attachment } from '../api/types';
import { server } from '../mocks/node';
import { renderWith } from '../test/render';
import { AttachmentList } from './AttachmentList';
import { AttachmentBody } from './AttachmentViewer';

const P = 'web-shop';
const CONTENT = '/api/v1/projects/:key/attachments/:id/content';

function a(id: string, name: string, contentType: string, kind: Attachment['kind']): Attachment {
  return { id, name, contentType, kind, size: 100 };
}

function serve(body: string, type: string) {
  server.use(http.get(CONTENT, () => new HttpResponse(body, { headers: { 'Content-Type': type } })));
}

const PNG = 'data:image/png;base64,iVBORw0KGgo=';

describe('attachment viewers', () => {
  it('renders an Allure image diff as a compare slider', async () => {
    serve(JSON.stringify({ expected: PNG, actual: PNG, diff: PNG }), 'application/vnd.allure.image.diff');
    renderWith(<AttachmentBody project={P} attachment={a('d', 'diff', 'application/vnd.allure.image.diff', 'imageDiff')} />);
    expect(await screen.findByTestId('image-compare')).toBeInTheDocument();
    expect(screen.getByAltText('diff: expected')).toHaveAttribute('src', PNG);
  });

  it('refuses SVG data URIs in an image diff', async () => {
    serve(JSON.stringify({ expected: 'data:image/svg+xml;base64,PHN2Zz4=', actual: PNG }), 'application/vnd.allure.image.diff');
    renderWith(<AttachmentBody project={P} attachment={a('d', 'diff', 'application/vnd.allure.image.diff', 'imageDiff')} />);
    expect(await screen.findByText(/has no expected and actual images/)).toBeInTheDocument();
  });

  it('renders an HTTP exchange with redacted headers and a JSON body', async () => {
    serve(
      JSON.stringify({
        request: { method: 'POST', url: 'https://api.example.com/x', headers: [{ name: 'Authorization', value: '__ALLURE_REDACTED__' }], body: { encoding: 'utf8', value: '{"a":1}' } },
        response: { status: 409, statusText: 'Conflict', headers: [{ name: 'Content-Type', value: 'application/json' }], body: { encoding: 'base64', value: btoa('{"code":"conflict"}') } },
      }),
      'application/vnd.allure.http+json',
    );
    const user = userEvent.setup();
    renderWith(<AttachmentBody project={P} attachment={a('h', 'POST /x', 'application/vnd.allure.http+json', 'httpExchange')} />);
    const view = await screen.findByTestId('http-exchange');
    expect(within(view).getByText('409 Conflict')).toBeInTheDocument();
    expect(within(view).getByText('"conflict"')).toBeInTheDocument();
    await user.click(within(view).getByRole('tab', { name: 'Request' }));
    expect(await within(view).findByText('redacted')).toBeInTheDocument();
  });

  it('renders logs with ANSI colors as spans, not markup', async () => {
    serve('\u001b[31mred\u001b[0m <b>not bold</b>', 'text/plain');
    renderWith(<AttachmentBody project={P} attachment={a('l', 'x.log', 'text/plain', 'log')} />);
    const pre = await screen.findByTestId('text-viewer');
    expect(pre.querySelector('b')).toBeNull();
    expect(pre.textContent).toBe('red <b>not bold</b>');
    expect(within(pre).getByText('red').getAttribute('style')).toContain('var(--fp-ansi-1)');
  });

  it('shows JSON as a collapsible tree', async () => {
    serve('{"a":{"b":[1,true,null]}}', 'application/json');
    renderWith(<AttachmentBody project={P} attachment={a('j', 'x.json', 'application/json', 'json')} />);
    const tree = await screen.findByTestId('json-tree');
    expect(tree.textContent).toContain('"a": {');
    expect(within(tree).getByText('true')).toBeInTheDocument();
  });

  it('never renders HTML or SVG inline and links to a new tab instead', () => {
    renderWith(<AttachmentBody project={P} attachment={a('h1', 'r.html', 'text/html', 'other')} />);
    const link = screen.getByTestId('open-new-tab');
    expect(link).toHaveAttribute('href', '/api/v1/projects/web-shop/attachments/h1/content');
    expect(link).toHaveAttribute('target', '_blank');
    expect(link.getAttribute('rel')).toContain('noopener');
    expect(document.querySelector('iframe')).toBeNull();
  });

  it('links a Playwright trace to the hosted trace viewer', () => {
    renderWith(<AttachmentBody project={P} attachment={a('t1', 'trace.zip', 'application/zip', 'trace')} />);
    expect(screen.getByTestId('trace-link').getAttribute('href')).toMatch(/^\/trace\/index\.html\?trace=http/);
  });

  it('lists attachments, groups Playwright triples and opens the viewer', async () => {
    const user = userEvent.setup();
    renderWith(
      <AttachmentList
        project={P}
        attachments={[a('e', 'cmp-expected.png', 'image/png', 'image'), a('ac', 'cmp-actual.png', 'image/png', 'image'), a('s', 'badge.svg', 'image/svg+xml', 'image'), a('v', 'video.webm', 'video/webm', 'video')]}
      />,
    );
    const items = screen.getAllByTestId('attachment-item');
    expect(items).toHaveLength(3);
    expect(items[1]).toHaveAttribute('target', '_blank');
    server.use(http.post('/api/v1/projects/web-shop/attachments/v/media-url', () => HttpResponse.json({ url: '/api/v1/media/tok', expiresAt: new Date(Date.now() + 900_000).toISOString() })));
    await user.click(screen.getByText('video.webm'));
    const viewer = await screen.findByTestId('video-viewer');
    expect(viewer).toHaveAttribute('src', '/api/v1/media/tok');
    await user.click(screen.getByText(/cmp \(expected/));
    await waitFor(() => expect(screen.getByTestId('image-compare')).toBeInTheDocument());
  });
});
