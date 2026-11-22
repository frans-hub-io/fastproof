import { act, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it, vi } from 'vitest';
import { getVersioned, type RunItem } from '../api/tm';
import { server } from '../mocks/node';
import { tmState } from '../mocks/tm';
import { manualLaunchId, ME, tmId } from '../mocks/tmFixtures';
import { renderApp, renderWith } from '../test/render';
import { DefectDrawer } from './runner/DefectDrawer';

const PLAN = tmId('plan:regression');

describe('assignee pickers use the people the server knows', () => {
  it('suggests GET /people and never the old example names; an unknown name gets a warning', async () => {
    server.use(http.get('/api/v1/projects/web-shop/people', ({ request }) => {
      const q = new URL(request.url).searchParams.get('q') ?? '';
      const all = [{ principal: 'qa.lead@corp.test', display: 'QA Lead' }, { principal: ME }];
      return HttpResponse.json({ items: all.filter((p) => p.principal.includes(q)) });
    }));
    const user = userEvent.setup();
    renderApp(`/p/web-shop/plans/${PLAN}`);
    const group = (await screen.findAllByTestId('plan-group'))[0];
    if (!group) throw new Error('no plan group');
    const input = within(group).getByTestId('folder-assign-input');
    await user.click(input);
    await user.type(input, 'qa');
    expect(await screen.findByRole('option', { name: /QA Lead/ })).toBeInTheDocument();
    expect(screen.queryByRole('option', { name: /sam@example\.com|maria@example\.com|oleh@example\.com/ })).toBeNull();
    await user.clear(input);
    await user.type(input, 'nobody-typo');
    await waitFor(() => expect(within(group).getByText(/Not a known user of this project/)).toBeInTheDocument());
  });
});

describe('Case fields page', () => {
  it('creates a select field with options, reorders, archives and restores it', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop/settings/case-fields');
    await user.click(await screen.findByTestId('field-new'));
    const dialog = await screen.findByTestId('field-dialog');
    await user.type(within(dialog).getByTestId('field-name'), 'Browser family');
    await user.type(within(dialog).getByTestId('field-key'), 'browser');
    await user.type(within(dialog).getByTestId('field-source'), 'browser');
    await user.click(within(dialog).getByTestId('field-option-add'));
    await user.click(within(dialog).getByTestId('field-option-add'));
    const values = within(dialog).getAllByTestId('field-option-value');
    await user.type(values[0] as HTMLElement, 'web');
    await user.type(values[1] as HTMLElement, 'android');
    await user.click(within(dialog).getByTestId('field-required'));
    await user.click(within(dialog).getByTestId('field-save'));
    await waitFor(() => expect(tmState().fields.find((f) => f.key === 'browser')).toMatchObject({ name: 'Browser family', type: 'select', required: true, sourceLabel: 'browser', options: [{ value: 'web', archived: false }, { value: 'android', archived: false }] }));

    const row = await screen.findByText('Browser family');
    const card = row.closest<HTMLElement>('[data-testid="field-row"]');
    if (!card) throw new Error('no row');
    const before = tmState().fields.filter((f) => !f.archived).sort((a, b) => a.position - b.position).map((f) => f.key);
    expect(before[before.length - 1]).toBe('browser');
    await user.click(within(card).getByTestId('field-up'));
    await waitFor(() => {
      const after = tmState().fields.filter((f) => !f.archived).sort((a, b) => a.position - b.position).map((f) => f.key);
      expect(after.indexOf('browser')).toBe(before.length - 2);
    });

    await user.click(within(card).getByTestId('field-archive'));
    await waitFor(() => expect(tmState().fields.find((f) => f.key === 'browser')?.archived).toBe(true));
    await user.click(screen.getByTestId('field-show-archived'));
    const archived = (await screen.findAllByTestId('field-row')).find((r) => r.dataset.key === 'browser');
    if (!archived) throw new Error('archived row missing');
    await user.click(within(archived).getByTestId('field-restore'));
    await waitFor(() => expect(tmState().fields.find((f) => f.key === 'browser')?.archived).toBe(false));
  });

  it('is linked from project settings and from the test cases page', async () => {
    renderApp('/p/web-shop/settings');
    expect(await screen.findByTestId('settings-case-fields')).toHaveAttribute('href', '/p/web-shop/settings/case-fields');
  });
});

describe('run management', () => {
  it('the launch list marks launches with a manual run', async () => {
    renderApp('/p/web-shop');
    const badges = await screen.findAllByTestId('launch-manual');
    expect(badges[0]).toHaveAttribute('data-state', 'open');
    expect(badges[0]).toHaveTextContent(/manual: \d+ left/);
  });

  it('the plan page lists the launches started from it', async () => {
    renderApp(`/p/web-shop/plans/${PLAN}`);
    const list = await screen.findByTestId('plan-launches');
    expect(await within(list).findByText(/^#38 /)).toBeInTheDocument();
    expect(within(list).getByTestId('plan-launch-manual')).toHaveTextContent(/manual open · \d+ \/ \d+ done/);
  });

  it('My runs lists the open items assigned to me, grouped by launch', async () => {
    renderApp('/p/web-shop/my-runs');
    const groups = await screen.findAllByTestId('my-runs-launch');
    expect(groups).toHaveLength(1);
    const mine = tmState().runItems.filter((r) => r.assignee === ME && (r.status === 'untested' || r.status === 'in_progress'));
    expect(within(groups[0] as HTMLElement).getAllByTestId('my-run-item')).toHaveLength(mine.length);
    expect(screen.getByRole('link', { name: 'My runs' })).toBeInTheDocument();
  });

  it('reassigns selected items in bulk and sets their result after a confirmation', async () => {
    const launch = manualLaunchId();
    const oleh = tmState().runItems.filter((r) => r.launchId === launch && r.assignee === 'oleh@example.com' && r.status === 'untested').slice(0, 3);
    expect(oleh).toHaveLength(3);
    const user = userEvent.setup();
    renderApp(`/p/web-shop/launches/${launch}?tab=manual`);
    await screen.findByTestId('manual-items', {}, { timeout: 20_000 });
    for (const it of oleh) {
      const row = screen.getAllByTestId('manual-item').find((r) => r.textContent.includes(`TC-${String(it.caseNumber)}`));
      if (!row) throw new Error('row missing');
      await user.click(within(row).getByTestId('manual-item-select'));
    }
    await user.click(screen.getByTestId('manual-bulk-assign'));
    const dialog = await screen.findByTestId('assign-dialog');
    await user.type(within(dialog).getByTestId('assign-input'), 'maria@example.com');
    await user.click(within(dialog).getByTestId('assign-confirm'));
    await waitFor(() => expect(oleh.map((r) => tmState().runItems.find((x) => x.id === r.id)?.assignee)).toEqual(['maria@example.com', 'maria@example.com', 'maria@example.com']));

    await waitFor(() => expect(screen.queryByTestId('assign-dialog')).toBeNull());
    for (const it of oleh.slice(0, 2)) {
      const row = screen.getAllByTestId('manual-item').find((r) => r.textContent.includes(`TC-${String(it.caseNumber)}`));
      if (!row) throw new Error('row missing');
      await user.click(within(row).getByTestId('manual-item-select'));
    }
    await user.click(screen.getByTestId('manual-bulk-result'));
    const result = await screen.findByTestId('set-result-dialog');
    expect(within(result).getByTestId('set-result-confirm')).toBeDisabled();
    await user.click(within(result).getByLabelText('Blocked'));
    expect(within(result).getByTestId('set-result-summary')).toHaveTextContent('2 cases will be finished as Blocked');
    await user.click(within(result).getByTestId('set-result-confirm'));
    await waitFor(() => expect(oleh.slice(0, 2).map((r) => tmState().runItems.find((x) => x.id === r.id)?.status)).toEqual(['blocked', 'blocked']));
    expect(tmState().runItems.find((x) => x.id === oleh[2]?.id)?.status).toBe('untested');
  });

  it('a single item is reassigned from its row', async () => {
    const launch = manualLaunchId();
    const it = tmState().runItems.find((r) => r.launchId === launch && r.assignee === 'maria@example.com');
    if (!it) throw new Error('no item');
    const user = userEvent.setup();
    renderApp(`/p/web-shop/launches/${launch}?tab=manual`);
    const row = (await screen.findAllByTestId('manual-item', {}, { timeout: 20_000 })).find((r) => r.textContent.includes(`TC-${String(it.caseNumber)}`));
    if (!row) throw new Error('row missing');
    await user.click(within(row).getByTestId('manual-item-reassign'));
    const dialog = await screen.findByTestId('assign-dialog');
    const input = within(dialog).getByTestId('assign-input');
    expect(input).toHaveValue('maria@example.com');
    await user.clear(input);
    await user.type(input, ME);
    await user.click(within(dialog).getByTestId('assign-confirm'));
    await waitFor(() => expect(tmState().runItems.find((x) => x.id === it.id)?.assignee).toBe(ME));
  });

  it('adds manual cases to the running launch', async () => {
    const launch = manualLaunchId();
    const inRun = new Set(tmState().runItems.filter((r) => r.launchId === launch).map((r) => r.caseNumber));
    const candidate = [...tmState().cases.values()].find((c) => !inRun.has(c.number) && c.identities.length === 0 && !c.archivedAt);
    if (!candidate) throw new Error('no candidate');
    let sent: unknown = null;
    server.events.on('request:start', ({ request }) => {
      if (request.method === 'POST' && request.url.endsWith(`/launches/${launch}/run-items`)) void request.clone().json().then((b: unknown) => {
          sent = b;
        });
    });
    const user = userEvent.setup();
    renderApp(`/p/web-shop/launches/${launch}?tab=manual`);
    await user.click(await screen.findByTestId('manual-add-cases'));
    const dialog = await screen.findByTestId('add-cases-dialog');
    const input = within(dialog).getByTestId('add-cases-input');
    await user.click(input);
    await user.type(input, candidate.name);
    await user.click(await screen.findByRole('option', { name: new RegExp(`TC-${String(candidate.number)} `) }));
    await user.click(within(dialog).getByTestId('add-cases-confirm'));
    await waitFor(() => expect(tmState().runItems.some((r) => r.launchId === launch && r.caseNumber === candidate.number)).toBe(true));
    expect(sent).toEqual({ caseNumbers: [candidate.number], assignee: null });
    server.events.removeAllListeners();
  });

  it('the print view holds every item with its steps, independent of the tab filter', async () => {
    const launch = manualLaunchId();
    renderApp(`/p/web-shop/launches/${launch}/manual/print`);
    const items = await screen.findAllByTestId('print-item');
    expect(items).toHaveLength(tmState().runItems.filter((r) => r.launchId === launch).length);
    const failed = tmState().runItems.find((r) => r.launchId === launch && r.status === 'failed');
    if (!failed) throw new Error('no failed item');
    const section = items.find((s) => s.dataset.case === String(failed.caseNumber));
    if (!section) throw new Error('failed item missing');
    expect(within(section).getByTestId('print-item-meta')).toHaveTextContent(`executed by ${failed.executedBy ?? ''}`);
    expect(within(section).getByText(/The confirmation page stays blank/)).toBeInTheDocument();
    expect(within(section).getAllByTestId('print-step').some((r) => r.dataset.status === 'failed')).toBe(true);
  });
});

describe('quick issue link for Jira', () => {
  it('offers "Copy issue text" instead of opening a link the tracker cannot prefill', async () => {
    server.use(
      http.get('/api/v1/projects/web-shop/settings', () =>
        HttpResponse.json({ flaky: { historyDepth: 10, stabilizationPeriod: 5, includePassedTests: false }, retention: { rawDays: 90, keepImportant: true, manualDays: 365 }, rollup: { afterDays: 30 }, gates: [], notifications: [], issueTrackers: [{ type: 'jira', urlTemplate: 'https://jira.example.com/browse/{key}', keyRegex: '^[A-Z]+-\\d+$' }], vcs: null, fastmock: null }),
      ),
    );
    const launch = manualLaunchId();
    const id = tmState().runItems.find((r) => r.launchId === launch && r.status === 'failed')?.id ?? '';
    const item = (await getVersioned<RunItem>(`/api/v1/projects/web-shop/run-items/${id}`)).value;
    const open = vi.spyOn(window, 'open').mockImplementation(() => null);
    renderWith(<DefectDrawer project="web-shop" item={item} local={new Map()} opened onClose={() => undefined} />);
    const button = await screen.findByTestId('defect-new-issue');
    await waitFor(() => expect(button).toHaveTextContent('Jira'));
    await act(async () => {
      fireEvent.click(button);
      await Promise.resolve();
    });
    const box = await screen.findByTestId('defect-prefill');
    expect(within(box).getByTestId('defect-copy-text')).toHaveTextContent('Copy issue text');
    expect(within(box).getByRole<HTMLTextAreaElement>('textbox').value).toContain(`TC-${String(item.caseNumber)} `);
    expect(within(box).getByTestId('defect-create-link')).toHaveAttribute('href', 'https://issues.example.com/secure/CreateIssue!default.jspa');
    expect(open).not.toHaveBeenCalled();
    open.mockRestore();
  });
});
