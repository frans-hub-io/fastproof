import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it, vi } from 'vitest';
import { notifications } from '@mantine/notifications';
import { server } from '../mocks/node';
import { reviewsState, SUGGESTED_WORKFLOW } from '../mocks/reviews';
import { putSettings } from '../mocks/stageC';
import { renderApp } from '../test/render';
import { validateWorkflow } from '../lib/tm/reviewUtil';

const POLICY = { mode: 'manual', approvals: 1, resetOn: 'content', allowSelfApproval: false, imports: 'review' } as const;

function reviewOn(workflow = false): void {
  putSettings('web-shop', { caseReview: POLICY } as never);
  if (workflow) reviewsState().workflow = structuredClone(SUGGESTED_WORKFLOW);
}

describe('validateWorkflow', () => {
  it('accepts the suggested workflow', () => {
    expect(validateWorkflow(SUGGESTED_WORKFLOW)).toEqual([]);
  });

  it('names bad keys, duplicates, colours, categories and guards', () => {
    const w = structuredClone(SUGGESTED_WORKFLOW);
    w.statuses[0] = { key: 'Bad', name: '', category: 'draft', color: 'red' };
    w.statuses[1] = { key: 'approved', name: 'x', category: 'draft' };
    w.statuses[2] = { key: 'approved', name: 'y', category: 'draft' };
    w.manual.transitions[0] = { from: ['approved'], to: 'in_review', requires: ['approved', 'requiredFields', 'notAuthor', 'approved'] };
    const errors = validateWorkflow(w).join('\n');
    expect(errors).toContain('Status 1: the key must start');
    expect(errors).toContain('Status 1: the name is 1 to 100 bytes');
    expect(errors).toContain('Status 1: the colour');
    expect(errors).toContain('used twice');
    expect(errors).toContain('active category');
    expect(errors).toContain('at most 3 conditions');
  });
});

describe('Workflow settings', () => {
  it('offers the suggested workflow, then the editor and the review policy', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop/settings?tab=workflow');
    await user.click(await screen.findByTestId('workflow-use-suggested'));
    expect(await screen.findByTestId('workflow-form')).toBeInTheDocument();
    expect(screen.getAllByTestId('workflow-status')).toHaveLength(4);
    expect(screen.getByTestId('policy-save')).toBeDisabled();
  });

  it('is read-only without settings:write', async () => {
    window.localStorage.setItem('fastproof-mock-role', 'viewer');
    reviewsState().workflow = structuredClone(SUGGESTED_WORKFLOW);
    renderApp('/p/web-shop/settings?tab=workflow');
    expect(await screen.findByTestId('workflow-form')).toBeInTheDocument();
    expect(screen.queryByTestId('workflow-save')).toBeNull();
    expect(screen.getAllByTestId('status-key')[0]).toBeDisabled();
    expect(screen.getByText(/Needs the settings:write permission/)).toBeInTheDocument();
  });

  it('asks where cases go on workflow_status_in_use and resends with remap', async () => {
    reviewsState().workflow = structuredClone(SUGGESTED_WORKFLOW);
    const bodies: unknown[] = [];
    server.use(
      http.put('/api/v1/projects/web-shop/case-workflow', async ({ request }) => {
        const b = (await request.json()) as { remap?: Record<string, string> };
        bodies.push(b);
        if (!b.remap) return HttpResponse.json({ code: 'workflow_status_in_use', message: 'in use', details: { statuses: [{ key: 'approved', cases: 3 }] } }, { status: 409 });
        return HttpResponse.json({ view: { configured: true, workflow: SUGGESTED_WORKFLOW, review: POLICY }, job: { id: 'j1', kind: 'workflow_remap', status: 'running', progress: { done: 1, total: 3 }, createdAt: new Date().toISOString() } });
      }),
    );
    const user = userEvent.setup();
    renderApp('/p/web-shop/settings?tab=workflow');
    const names = await screen.findAllByTestId('status-name');
    await user.type(names[0] as HTMLElement, ' x');
    await user.click(screen.getByTestId('workflow-save'));
    expect(await screen.findByTestId('remap-dialog')).toBeInTheDocument();
    expect(screen.getByText(/approved: 3 cases move to/)).toBeInTheDocument();
    expect(screen.getByTestId('remap-confirm')).toBeDisabled();
    await user.click(screen.getByTestId('remap-select'));
    await user.click(await screen.findByRole('option', { name: /In review \(in_review\)/ }));
    await user.click(screen.getByTestId('remap-confirm'));
    await waitFor(() => expect(bodies).toHaveLength(2));
    expect(bodies[1]).toMatchObject({ remap: { approved: 'in_review' } });
    expect(await screen.findByTestId('remap-job')).toHaveTextContent('1 of 3');
  });
});

describe('Case review tab and state chip', () => {
  it('says review is off, with a link for admins', async () => {
    renderApp('/p/web-shop/cases/2?tab=review');
    expect(await screen.findByTestId('review-off')).toBeInTheDocument();
    expect(screen.getByTestId('review-off-link')).toHaveAttribute('href', '/p/web-shop/settings?tab=workflow');
    expect(screen.queryByTestId('state-chip')).toBeNull();
  });

  it('approves a version and shows the comment as plain text', async () => {
    reviewOn();
    const user = userEvent.setup();
    renderApp('/p/web-shop/cases/2?tab=review');
    expect(await screen.findByTestId('review-state')).toHaveAttribute('data-state', 'pending');
    expect(screen.getByTestId('review-tab')).toHaveTextContent('New case');
    await user.type(screen.getByTestId('review-comment'), '<img src=x onerror=alert(1)>');
    await user.click(screen.getByTestId('review-approve'));
    await waitFor(() => expect(screen.getByTestId('review-state')).toHaveAttribute('data-state', 'approved'));
    expect(screen.getByTestId('vote-comment').textContent).toBe('<img src=x onerror=alert(1)>');
    expect(screen.getByTestId('vote-comment').querySelector('img')).toBeNull();
  });

  it('explains why the author cannot approve', async () => {
    reviewOn();
    renderApp('/p/web-shop/cases/4?tab=review');
    expect(await screen.findByTestId('review-why')).toHaveTextContent('cannot approve your own change');
    expect(screen.getByTestId('review-approve')).toBeDisabled();
  });

  it('tells the user when the version moved and reloads the thread', async () => {
    reviewOn();
    server.use(http.post('/api/v1/projects/web-shop/cases/2/reviews', () => HttpResponse.json({ code: 'version_moved', message: 'moved', details: { currentVersion: 2 } }, { status: 409 })));
    const user = userEvent.setup();
    renderApp('/p/web-shop/cases/2?tab=review');
    await user.click(await screen.findByTestId('review-approve'));
    expect(await screen.findByTestId('review-moved')).toBeInTheDocument();
  });

  it('a viewer sees the permission reason', async () => {
    reviewOn();
    window.localStorage.setItem('fastproof-mock-role', 'viewer');
    renderApp('/p/web-shop/cases/2?tab=review');
    expect(await screen.findByTestId('review-why')).toHaveTextContent('cases:review');
    expect(screen.getByTestId('review-approve')).toBeDisabled();
  });

  it('lists the moves from the state with readable reasons for the blocked ones', async () => {
    reviewOn(true);
    const user = userEvent.setup();
    renderApp('/p/web-shop/cases/2');
    const menu = await screen.findByTestId('state-menu');
    expect(screen.getByTestId('state-chip')).toHaveAttribute('data-state', 'approved');
    await user.click(menu);
    const options = await screen.findAllByTestId('state-option');
    expect(options.map((o) => o.getAttribute('data-state'))).toEqual(['retired']);
    expect(options[0]).toHaveAttribute('data-allowed', 'true');
    await user.click(options[0] as HTMLElement);
    await waitFor(() => expect(screen.getByTestId('state-chip')).toHaveAttribute('data-state', 'retired'));
    await user.click(screen.getByTestId('state-menu'));
    expect((await screen.findAllByTestId('state-option')).map((o) => o.getAttribute('data-state'))).toEqual(['draft']);
  });

  it('shows the 409 message when a move is refused', async () => {
    reviewOn(true);
    server.use(http.post('/api/v1/projects/web-shop/cases/2/transition', () => HttpResponse.json({ code: 'invalid_transition', message: 'cannot move', details: { from: 'approved', to: 'retired', missing: ['permission'] } }, { status: 409 })));
    const spy = vi.spyOn(notifications, 'show');
    const user = userEvent.setup();
    renderApp('/p/web-shop/cases/2');
    await user.click(await screen.findByTestId('state-menu'));
    await user.click((await screen.findAllByTestId('state-option'))[0] as HTMLElement);
    await waitFor(() => expect(spy).toHaveBeenCalledWith(expect.objectContaining({ color: 'red', message: expect.stringContaining('you lack the permission this move needs') as string })));
    expect(screen.getByTestId('state-chip')).toHaveAttribute('data-state', 'approved');
    spy.mockRestore();
  });
});

describe('Reviews page', () => {
  it('says review is off', async () => {
    renderApp('/p/web-shop/reviews');
    expect(await screen.findByText('Review is off for this project')).toBeInTheDocument();
    expect(screen.getByTestId('reviews-off-link')).toBeInTheDocument();
  });

  it('says nothing waits when the queue is empty', async () => {
    reviewOn();
    server.use(http.get('/api/v1/projects/web-shop/reviews', () => HttpResponse.json({ items: [], next: null })));
    renderApp('/p/web-shop/reviews');
    expect(await screen.findByText('Nothing waits for review')).toBeInTheDocument();
  });

  it('lists the queue with links, and loads more', async () => {
    reviewOn();
    const user = userEvent.setup();
    renderApp('/p/web-shop/reviews');
    const rows = await screen.findAllByTestId('review-row');
    expect(rows).toHaveLength(50);
    const link = within(rows[0] as HTMLElement).getByTestId('review-link');
    expect(link.getAttribute('href')).toMatch(/^\/p\/web-shop\/cases\/\d+\?tab=review$/);
    await user.click(screen.getByTestId('reviews-more'));
    await waitFor(() => expect(screen.getAllByTestId('review-row').length).toBeGreaterThan(50));
  });

  it('reviews a batch and lists the skipped items with readable reasons', async () => {
    reviewOn();
    const user = userEvent.setup();
    renderApp('/p/web-shop/reviews?view=batch');
    await user.click(await screen.findByTestId('batch-load'));
    expect((await screen.findAllByTestId('batch-item')).length).toBeGreaterThan(1);
    await user.click(screen.getByTestId('batch-request'));
    const result = await screen.findByTestId('batch-result');
    expect(result).toHaveTextContent(/decided, \d+ skipped/);
    const skipped = within(result).getAllByTestId('batch-skipped');
    expect(skipped[0]).toHaveAttribute('data-code', 'self_approval');
    expect(skipped[0]).toHaveTextContent('your own change');
  });

  it('says queries are not available when the server has none yet', async () => {
    reviewOn();
    server.use(http.get('/api/v1/projects/web-shop/cases', () => HttpResponse.json({ code: 'invalid_query', message: 'queries are not available' }, { status: 400 })));
    const user = userEvent.setup();
    renderApp('/p/web-shop/reviews?view=batch');
    await user.click(await screen.findByLabelText('FPQL query', { selector: 'input[type=radio]' }).catch(() => screen.getByText('FPQL query')));
    await user.type(await screen.findByTestId('batch-query'), 'tag = "x"');
    await user.click(screen.getByTestId('batch-load'));
    expect(await screen.findByTestId('batch-note')).toHaveTextContent('Queries are not available');
  });

  it('a viewer cannot decide in a batch', async () => {
    reviewOn();
    window.localStorage.setItem('fastproof-mock-role', 'viewer');
    const user = userEvent.setup();
    renderApp('/p/web-shop/reviews?view=batch');
    await user.click(await screen.findByTestId('batch-load'));
    await screen.findAllByTestId('batch-item');
    expect(screen.queryByTestId('batch-approve')).toBeNull();
    expect(screen.getByText(/You need the cases:review permission/)).toBeInTheDocument();
  });
});
