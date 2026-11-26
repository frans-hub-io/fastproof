import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { manualLaunchId } from '../../mocks/tmFixtures';
import { renderApp } from '../../test/render';

const P = '/api/v1/projects/web-shop';

async function call(method: string, url: string, body?: unknown, headers: Record<string, string> = {}): Promise<{ status: number; etag: string | null; body: Record<string, unknown> }> {
  const r = await fetch(url, { method, headers: { 'Content-Type': 'application/json', ...headers }, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await r.text();
  return { status: r.status, etag: r.headers.get('ETag'), body: (text ? JSON.parse(text) : {}) as Record<string, unknown> };
}

/** A manual case with two enabled rows and one disabled, in the seeded manual run; returns its number and item id. */
async function parameterizedItem(): Promise<{ n: number; item: string }> {
  const c = await call('POST', `${P}/cases`, {
    name: 'Pay in every currency',
    status: 'draft',
    body: {
      description: '',
      preconditions: 'Open the shop in ${browser}',
      links: [],
      steps: [
        { id: 's_add', action: 'Add a book', expected: 'The cart has one book', data: '', attachments: [], steps: [] },
        { id: 's_pay', action: 'Pay in **${currency}**', expected: 'Paid in ${currency}', data: '', attachments: [], steps: [] },
      ],
      parameters: {
        columns: ['browser', 'currency'],
        rows: [
          { id: 'r_ff', values: { browser: 'firefox', currency: 'EUR' }, enabled: true },
          { id: 'r_off', values: { browser: 'safari', currency: 'GBP' }, enabled: false },
          { id: 'r_cr', values: { browser: 'chromium', currency: 'USD' }, enabled: true },
        ],
      },
    },
  });
  expect(c.status).toBe(201);
  const n = Number(c.body.number);
  const launch = manualLaunchId();
  const added = await call('POST', `${P}/launches/${launch}/run-items`, { caseNumbers: [n] });
  expect(added.body.added).toBe(1);
  const list = await call('GET', `${P}/launches/${launch}/run-items?limit=500`);
  const items = list.body.items as { id: string; caseNumber: number; iterationCount: number }[];
  const it = items.find((x) => x.caseNumber === n);
  expect(it?.iterationCount).toBe(2);
  return { n, item: it?.id ?? '' };
}

describe('parameters', () => {
  it('runs a parameterized case one iteration at a time with its values in the steps', async () => {
    const user = userEvent.setup();
    const { item } = await parameterizedItem();
    renderApp(`/p/web-shop/launches/${manualLaunchId()}/run/${item}`);
    const strip = await screen.findByTestId('iteration-strip', {}, { timeout: 20_000 });
    expect(within(strip).getAllByTestId('iteration-chip')).toHaveLength(2);
    expect(screen.queryByTestId('finish-panel')).toBeNull();
    const iteration = screen.getByTestId('iteration');
    expect(iteration).toHaveAttribute('data-row', 'r_ff');
    expect(within(iteration).getByTestId('iteration-values')).toHaveTextContent('EUR');
    await waitFor(() => expect(iteration).toHaveTextContent('Pay in EUR'));
    expect(iteration).toHaveTextContent('Open the shop in firefox');
    expect(iteration).not.toHaveTextContent('${currency}');

    const steps = within(iteration).getAllByTestId('iteration-step');
    await user.click(within(steps[0] as HTMLElement).getByTestId('set-passed'));
    await waitFor(async () => {
      const v = await call('GET', `${P}/run-items/${item}`);
      expect(v.body.status).toBe('in_progress');
    });
    await user.click(screen.getByTestId('iteration-finish-passed'));
    await waitFor(() => expect(screen.getByTestId('iteration')).toHaveAttribute('data-row', 'r_cr'));
    expect(screen.getByTestId('iteration')).toHaveTextContent('Pay in USD');
    const chips = within(screen.getByTestId('iteration-strip')).getAllByTestId('iteration-chip');
    expect(chips[0]).toHaveAttribute('data-status', 'passed');

    await user.click(screen.getByTestId('iteration-finish-failed'));
    await waitFor(async () => {
      const v = await call('GET', `${P}/run-items/${item}`);
      expect(v.body.status).toBe('failed');
    });

    await user.keyboard('[[');
    await waitFor(() => expect(screen.getByTestId('iteration')).toHaveAttribute('data-row', 'r_ff'));
    await user.keyboard(']');
    await waitFor(() => expect(screen.getByTestId('iteration')).toHaveAttribute('data-row', 'r_cr'));

    await user.click(screen.getByTestId('iterations-apply-all'));
    await user.click(await screen.findByTestId('apply-all-skipped'));
    await user.click(await screen.findByTestId('apply-all-ok'));
    await waitFor(async () => {
      const v = await call('GET', `${P}/run-items/${item}`);
      expect(v.body.status).toBe('skipped');
      expect((v.body.iterations as { status: string }[]).every((x) => x.status === 'skipped')).toBe(true);
    });
  }, 60_000);

  it('refuses item-level writes and needs If-Match on iterations', async () => {
    const { item } = await parameterizedItem();
    const step = await call('PUT', `${P}/run-items/${item}/steps/1`, { status: 'passed' });
    expect([step.status, step.body.code]).toEqual([409, 'parameterized_item']);
    const noMatch = await call('PATCH', `${P}/run-items/${item}/iterations/r_ff`, { status: 'passed' });
    expect(noMatch.status).toBe(428);
    const cur = await call('GET', `${P}/run-items/${item}`);
    const etag = cur.etag ?? '';
    const ok = await call('PATCH', `${P}/run-items/${item}/iterations/r_ff`, { status: 'passed' }, { 'If-Match': etag });
    expect(ok.status).toBe(200);
    const stale = await call('PATCH', `${P}/run-items/${item}/iterations/r_cr`, { status: 'passed' }, { 'If-Match': etag });
    expect(stale.status).toBe(412);
    const item2 = await call('PATCH', `${P}/run-items/${item}`, { status: 'passed' }, { 'If-Match': ok.etag ?? '' });
    expect(item2.body.code).toBe('parameterized_item');
  });

  it('shows, edits and saves the parameters of a case, and flags unknown placeholders', async () => {
    const user = userEvent.setup();
    const { n } = await parameterizedItem();
    renderApp(`/p/web-shop/cases/${String(n)}?tab=parameters`);
    const tab = await screen.findByTestId('parameters-tab', {}, { timeout: 20_000 });
    expect(within(tab).getByTestId('parameters-iterations')).toHaveTextContent('2 iterations per run');
    expect(within(tab).getByTestId('parameters-view')).toHaveTextContent('chromium');
    await user.click(within(tab).getByTestId('parameters-edit'));
    const editor = await screen.findByTestId('parameters-editor');
    await user.click(within(editor).getByTestId('parameters-add-row'));
    const rows = within(editor).getAllByTestId('parameters-row');
    expect(rows).toHaveLength(4);
    const last = rows[3] as HTMLElement;
    await user.type(within(last).getByLabelText('browser in row 4'), 'edge');
    await user.type(within(last).getByLabelText('currency in row 4'), 'UAH');
    await user.click(screen.getByTestId('parameters-save'));
    await waitFor(() => expect(screen.getByTestId('parameters-iterations')).toHaveTextContent('3 iterations per run'));
    const saved = await call('GET', `${P}/cases/${String(n)}`);
    const params = (saved.body.body as { parameters: { rows: { values: Record<string, string>; id: string }[] } }).parameters;
    expect(params.rows[3]?.values).toEqual({ browser: 'edge', currency: 'UAH' });
    expect(params.rows[3]?.id).toMatch(/^r_/);

    await user.click(screen.getByTestId('parameters-edit'));
    const col = within(await screen.findByTestId('parameters-editor')).getAllByTestId('parameters-column')[1] as HTMLElement;
    const input = col.querySelector('input') ?? col;
    await user.clear(input);
    await user.type(input, 'money');
    expect(await screen.findByTestId('parameters-unknown')).toHaveTextContent('${currency}');
  }, 60_000);

  it('creates a defect on one iteration and on the whole item, with the row, values and failing step in the text', async () => {
    const user = userEvent.setup();
    const { item } = await parameterizedItem();
    const put = (row: string, body: unknown) => call('PUT', `${P}/run-items/${item}/iterations/${row}/steps/2`, body);
    expect((await put('r_cr', { status: 'failed', actual: 'card declined' })).status).toBe(200);
    for (const row of ['r_ff', 'r_cr']) {
      const cur = await call('GET', `${P}/run-items/${item}`);
      expect((await call('PATCH', `${P}/run-items/${item}/iterations/${row}`, { status: row === 'r_ff' ? 'passed' : 'failed' }, { 'If-Match': cur.etag ?? '' })).status).toBe(200);
    }
    renderApp(`/p/web-shop/launches/${manualLaunchId()}/run/${item}`);
    await screen.findByTestId('iteration-strip', {}, { timeout: 20_000 });
    await user.click(within(screen.getByTestId('iteration-strip')).getAllByTestId('iteration-chip')[1] as HTMLElement);
    await waitFor(() => expect(screen.getByTestId('iteration')).toHaveAttribute('data-row', 'r_cr'));
    await user.click(screen.getByTestId('iteration-defect'));
    const drawer = await screen.findByTestId('defect-drawer');
    expect(within(drawer).getByTestId('defect-scope')).toHaveTextContent('iteration #2');
    const text = within(drawer).getByTestId<HTMLTextAreaElement>('defect-comment');
    expect(text.value).toContain('Iteration #2: browser=chromium · currency=USD (failed)');
    expect(text.value).toContain('Step 2 failed: Pay in **USD**');
    expect(text.value).toContain('Actual: card declined');
    await user.type(within(drawer).getByTestId('defect-issue'), 'PAY-7');
    await user.click(within(drawer).getByTestId('defect-save'));
    await waitFor(async () => {
      const v = await call('GET', `${P}/run-items/${item}`);
      const its = v.body.iterations as { rowId: string; defect: { issue?: { key?: string } } | null }[];
      expect(its.find((x) => x.rowId === 'r_cr')?.defect?.issue?.key).toBe('PAY-7');
      expect(its.find((x) => x.rowId === 'r_ff')?.defect).toBeNull();
    });
    await waitFor(() => expect(screen.getByTestId('iteration-defect-info')).toHaveTextContent('Defect'));
    expect(screen.getByTestId('item-defect-info')).toBeInTheDocument();

    await user.click(screen.getByTestId('item-defect'));
    const whole = await screen.findByTestId('defect-drawer');
    expect(within(whole).getByTestId('defect-scope')).toHaveTextContent('every failed or blocked iteration');
    expect(within(whole).getByTestId<HTMLTextAreaElement>('defect-comment').value).toContain('1 of 2 iterations not passed');

    const link = await call('GET', `${P}/run-items/${item}/new-issue-url?tracker=jira&rowId=r_cr`);
    expect((link.body.prefill as { description: string }).description).toContain('Iteration #2: browser=chromium');
    const missing = await call('GET', `${P}/run-items/${item}/new-issue-url?tracker=jira&rowId=r_x`);
    expect(missing.status).toBe(404);
    const open = await call('POST', `${P}/run-items/${item}/defect`, { rowId: 'r_x', comment: 'c' });
    expect(open.status).toBe(404);
  }, 60_000);

  it('shows the iterations in the manual report and in the earlier runs of the item', async () => {
    const { item } = await parameterizedItem();
    expect((await call('PUT', `${P}/run-items/${item}/iterations/r_cr/steps/2`, { status: 'failed', actual: 'card declined' })).status).toBe(200);
    for (const row of ['r_ff', 'r_cr']) {
      const cur = await call('GET', `${P}/run-items/${item}`);
      await call('PATCH', `${P}/run-items/${item}/iterations/${row}`, { status: row === 'r_ff' ? 'passed' : 'failed', comment: row === 'r_cr' ? 'EUR only' : '' }, { 'If-Match': cur.etag ?? '' });
    }
    const report = await call('GET', `${P}/launches/${manualLaunchId()}/manual-report?format=json`);
    const mine = (report.body.items as { parameterColumns?: string[]; iterations?: { values: Record<string, string>; steps: { action: string }[] }[] }[]).find((x) => x.iterations?.length);
    expect(mine?.parameterColumns).toEqual(['browser', 'currency']);
    expect(mine?.iterations?.[1]?.steps[0]?.action).toBe('Pay in **USD**');

    const view = renderApp(`/p/web-shop/launches/${manualLaunchId()}/manual/print`);
    const rows = await screen.findAllByTestId('print-iteration', {}, { timeout: 20_000 });
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveAttribute('data-status', 'passed');
    expect(rows[1]).toHaveAttribute('data-status', 'failed');
    expect(rows[1]).toHaveTextContent('browser=chromium · currency=USD');
    expect(rows[1]).toHaveTextContent('card declined');
    expect(rows[1]).toHaveTextContent('EUR only');
    view.unmount();

    const cur = await call('GET', `${P}/run-items/${item}`);
    expect((await call('POST', `${P}/run-items/${item}/rerun`, undefined, { 'If-Match': cur.etag ?? '' })).status).toBe(200);
    renderApp(`/p/web-shop/launches/${manualLaunchId()}/run/${item}`);
    const earlier = await screen.findByTestId('execution-iterations', {}, { timeout: 20_000 });
    const its = within(earlier).getAllByTestId('execution-iteration');
    expect(its).toHaveLength(2);
    expect(its[1]).toHaveAttribute('data-status', 'failed');
    expect(its[1]).toHaveTextContent('browser=chromium · currency=USD');
    expect(its[1]).toHaveTextContent('Step 2 failed: card declined');
    expect(screen.getByTestId('iteration')).toHaveAttribute('data-status', 'untested');
  }, 60_000);

  it('reads the step results of a later iteration when the item leaves them out', async () => {
    const rows = Array.from({ length: 30 }, (_, i) => ({ id: `r_${String(i)}`, values: { browser: `b${String(i)}` }, enabled: true }));
    const c = await call('POST', `${P}/cases`, {
      name: 'Many rows',
      status: 'draft',
      body: { description: '', preconditions: '', links: [], steps: [{ id: 's_1', action: 'Open ${browser}', expected: '', data: '', attachments: [], steps: [] }], parameters: { columns: ['browser'], rows } },
    });
    expect(c.status).toBe(201);
    const launch = manualLaunchId();
    await call('POST', `${P}/launches/${launch}/run-items`, { caseNumbers: [Number(c.body.number)] });
    const list = await call('GET', `${P}/launches/${launch}/run-items?limit=500`);
    const item = (list.body.items as { id: string; caseNumber: number }[]).find((x) => x.caseNumber === Number(c.body.number))?.id ?? '';
    expect((await call('PUT', `${P}/run-items/${item}/iterations/r_29/steps/1`, { status: 'failed', actual: 'late failure' })).status).toBe(200);
    const view = await call('GET', `${P}/run-items/${item}`);
    const last = (view.body.iterations as { rowId: string; stepResults: unknown[]; stepResultsOmitted?: boolean }[])[29];
    expect(last?.stepResults).toEqual([]);
    expect(last?.stepResultsOmitted).toBe(true);
    const one = await call('GET', `${P}/run-items/${item}/iterations/r_29`);
    expect((one.body.stepResults as { actual: string }[])[0]?.actual).toBe('late failure');

    const user = userEvent.setup();
    renderApp(`/p/web-shop/launches/${launch}/run/${item}`);
    await screen.findByTestId('iteration-strip', {}, { timeout: 20_000 });
    await user.click(within(screen.getByTestId('iteration-strip')).getAllByTestId('iteration-chip')[29] as HTMLElement);
    await waitFor(() => expect(screen.getByTestId('iteration')).toHaveAttribute('data-row', 'r_29'));
    const step = await screen.findByTestId('iteration-step');
    await waitFor(() => expect(step).toHaveAttribute('data-status', 'failed'));
    expect(screen.queryByTestId('iteration-loading')).toBeNull();
    expect(screen.getByTestId<HTMLTextAreaElement>('iteration-actual').value).toBe('late failure');
  }, 60_000);

  it('puts a limit on what the values add to the texts of an iteration', async () => {
    const value = 'v'.repeat(1000);
    const c = await call('POST', `${P}/cases`, {
      name: 'Greedy',
      status: 'draft',
      body: { description: '', preconditions: '', links: [], steps: [{ id: 's_1', action: '${a} '.repeat(3000), expected: '', data: '', attachments: [], steps: [] }], parameters: { columns: ['a'], rows: [{ id: 'r_1', values: { a: value }, enabled: true }] } },
    });
    expect(c.status).toBe(201);
    const launch = manualLaunchId();
    await call('POST', `${P}/launches/${launch}/run-items`, { caseNumbers: [Number(c.body.number)] });
    const list = await call('GET', `${P}/launches/${launch}/run-items?limit=500`);
    const item = (list.body.items as { id: string; caseNumber: number }[]).find((x) => x.caseNumber === Number(c.body.number))?.id ?? '';
    renderApp(`/p/web-shop/launches/${launch}/run/${item}`);
    const step = await screen.findByTestId('iteration-step', {}, { timeout: 20_000 });
    await waitFor(() => expect(step.textContent.length).toBeGreaterThan(900_000));
    expect(step.textContent.length).toBeLessThan(1_200_000);
    expect(step.textContent).toContain('${a}');
  }, 60_000);
});
