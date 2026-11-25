import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';
import { ciBody } from '../lib/tm/ci';
import { cleanRerunPlanFields, rerunPlanErrors } from '../lib/tm/rerunPlan';
import { server } from '../mocks/node';
import { putSettings, settingsOf } from '../mocks/stageC';
import { tmId } from '../mocks/tmFixtures';
import { renderApp } from '../test/render';

const PLAN = tmId('plan:regression');

function allowCi(): void {
  const rerun = settingsOf('web-shop').rerun;
  if (!rerun) throw new Error('web-shop has rerun settings');
  putSettings('web-shop', {
    rerun: { ...rerun, refs: ['main', 'release/*'], variables: [{ name: 'BROWSER', values: ['chromium', 'firefox'], default: 'chromium', description: 'Browser' }, { name: 'SUITE' }] },
  });
}

describe('CI body of a plan start', () => {
  it('leaves out empty values and sends only what was set', () => {
    expect(ciBody({}, '', [])).toEqual({ dispatch: true });
    expect(ciBody({ BROWSER: 'firefox', SUITE: '' }, 'release/1', ['stage', 'prod'])).toEqual({
      dispatch: true,
      ref: 'release/1',
      variables: { BROWSER: 'firefox' },
      environments: [{ name: 'stage' }, { name: 'prod' }],
    });
  });
});

describe('rerun plan fields', () => {
  const base = { provider: 'github' as const, url: 'https://api.github.com', repository: 'a/b', workflow: 't.yml', tokenSecretRef: 's' };

  it('names what is wrong with names, defaults and refs', () => {
    expect(rerunPlanErrors({ ...base, variables: [{ name: 'BROWSER', values: ['a'], default: 'a' }, { name: 'SUITE' }] }).variables).toEqual([null, null]);
    const e = rerunPlanErrors({ ...base, refs: ['ok/*', 'bad ref'], variables: [{ name: 'lower' }, { name: 'FASTPROOF_X' }, { name: 'A' }, { name: 'A' }, { name: 'B', values: ['x'], default: 'y' }] });
    expect(e.refs).not.toBeNull();
    expect(rerunPlanErrors({ ...base, variables: [{ name: 'CI_DEBUG_TRACE' }, { name: 'LD_PRELOAD' }, { name: 'PATH' }] }).variables.every(Boolean)).toBe(true);
    expect(e.variables.map((x) => x !== null)).toEqual([true, true, false, true, true]);
  });

  it('sends nothing for empty lists, so the stored document stays as it was', () => {
    const out = cleanRerunPlanFields({ ...base, refs: [], variables: [] });
    expect('refs' in out || 'variables' in out).toBe(false);
    const full = cleanRerunPlanFields({ ...base, variables: [{ name: 'A', values: [], default: '', description: '' }] });
    expect(full.variables).toEqual([{ name: 'A' }]);
  });
});

describe('Start launch in CI', () => {
  it('starts one launch per environment with the chosen variables and shows every dispatch', async () => {
    allowCi();
    const user = userEvent.setup();
    renderApp(`/p/web-shop/plans/${PLAN}`);
    await user.click(await screen.findByTestId('plan-start'));
    const dialog = await screen.findByTestId('start-dialog');
    await user.click(await within(dialog).findByTestId('start-ci'));
    await user.type(within(dialog).getByTestId('ci-envs'), 'stage,broken,');
    await user.type(within(dialog).getByTestId('ci-var-SUITE'), 'smoke');
    await user.click(within(dialog).getByTestId('ci-var-BROWSER'));
    await user.click(await screen.findByRole('option', { name: 'firefox' }));
    await user.type(within(dialog).getByTestId('ci-ref'), 'release/2');
    expect(within(dialog).queryByText('External id for CI')).toBeNull();
    await user.click(within(dialog).getByTestId('start-submit'));
    const done = await screen.findByTestId('launch-started');
    const rows = within(done).getAllByTestId('ci-result');
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveTextContent('dispatched');
    expect(rows[0]).toHaveTextContent('stage');
    expect(rows[1]).toHaveTextContent('failed');
    expect(rows[1]).toHaveTextContent('the CI answered HTTP 422');
    expect(within(done).getByTestId('ci-failed')).toHaveTextContent('1 of 2');
    expect(within(done).getByRole('link', { name: /Pipeline/ })).toHaveAttribute('href', expect.stringMatching(/^https:\/\/ci\.example\.com\//));
    await user.keyboard('{Escape}');
    const list = await screen.findByTestId('plan-dispatches');
    const first = within(list).getAllByTestId('plan-dispatch')[0];
    expect(first).toHaveTextContent('BROWSER=firefox SUITE=smoke');
    expect(first).toHaveTextContent('release/2');
  });

  it('shows a refusal of the server and keeps the dialog open', async () => {
    allowCi();
    server.use(http.post('/api/v1/projects/web-shop/test-plans/:id/launches', () => HttpResponse.json({ code: 'ci_variable_not_allowed', message: 'CI variable HOME is not allowed', details: { name: 'HOME' } }, { status: 400 })));
    const user = userEvent.setup();
    renderApp(`/p/web-shop/plans/${PLAN}`);
    await user.click(await screen.findByTestId('plan-start'));
    const dialog = await screen.findByTestId('start-dialog');
    await user.click(await within(dialog).findByTestId('start-ci'));
    await user.click(within(dialog).getByTestId('start-submit'));
    expect(await screen.findByTestId('start-error')).toHaveTextContent('CI variable HOME is not allowed');
    expect(screen.getByTestId('start-dialog')).toBeInTheDocument();
  });

  it('offers no CI section without rerun settings', async () => {
    putSettings('web-shop', { rerun: null });
    const user = userEvent.setup();
    renderApp(`/p/web-shop/plans/${PLAN}`);
    await user.click(await screen.findByTestId('plan-start'));
    const dialog = await screen.findByTestId('start-dialog');
    await within(dialog).findByTestId('start-automated');
    await waitFor(() => expect(within(dialog).queryByTestId('start-ci')).toBeNull());
  });

  it('lists the dispatches on the plan page and warns on the launch page', async () => {
    allowCi();
    const user = userEvent.setup();
    const { unmount } = renderApp(`/p/web-shop/plans/${PLAN}`);
    await user.click(await screen.findByTestId('plan-start'));
    const dialog = await screen.findByTestId('start-dialog');
    await user.click(await within(dialog).findByTestId('start-ci'));
    await user.click(within(dialog).getByTestId('start-submit'));
    const done = await screen.findByTestId('launch-started');
    const link = within(done).getAllByRole('link').find((a) => /\/launches\//.test(a.getAttribute('href') ?? ''));
    if (!link) throw new Error('no launch link');
    const launchPath = link.getAttribute('href') ?? '';
    await user.keyboard('{Escape}');
    const list = await screen.findByTestId('plan-dispatches');
    expect(within(list).getAllByTestId('plan-dispatch')).toHaveLength(1);
    expect(within(list).getByTestId('dispatch-status')).toHaveTextContent('dispatched');
    expect(within(list).getByText(/BROWSER=chromium/)).toBeInTheDocument();
    unmount();
    renderApp(launchPath);
    const banner = await screen.findByTestId('plan-check', {}, { timeout: 5000 });
    expect(banner).toHaveTextContent('does not match its plan');
    expect(within(banner).getByTestId('plan-check-extra')).toHaveTextContent('checkout.legacy_flow');
  });
});
