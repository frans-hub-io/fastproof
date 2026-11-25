import { createRef, useState } from 'react';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { SharedStepBrief } from '../api/sharedSteps';
import { getVersioned, type Case, type CaseStep, type RunItem } from '../api/tm';
import { resetSharedSteps } from '../mocks/sharedSteps';
import { server } from '../mocks/node';
import { tmState } from '../mocks/tm';
import { renderApp, renderWith } from '../test/render';
import { StepsEditor } from './cases/StepsEditor';
import { ScenarioView } from './cases/ScenarioView';
import { RunSteps } from './runner/RunSteps';

beforeEach(() => {
  resetSharedSteps();
});

const step = (id: string, action: string, steps: CaseStep[] = []): CaseStep => ({ id, action, expected: '', data: '', attachments: [], steps });

function brief(over: Partial<SharedStepBrief> = {}): SharedStepBrief {
  const steps = [step('s_a', 'Open the **login** page'), step('s_b', 'Sign in', [step('s_b1', 'Type the user')])];
  return {
    id: '0190a1b2-c3d4-7e5f-8a9b-000000000001',
    number: 1,
    name: 'Log in as a customer',
    version: 2,
    archived: false,
    steps,
    stepsHtml: [
      { id: 's_a', action: '<p>Open the <strong>login</strong> page</p>', expected: '', data: '', steps: [] },
      { id: 's_b', action: '<p>Sign in</p>', expected: '', data: '', steps: [{ id: 's_b1', action: '<p>Type the user</p>', expected: '', data: '', steps: [] }] },
    ],
    ...over,
  };
}

const ref = (b: SharedStepBrief, id = 's_ref'): CaseStep => ({ ...step(id, ''), shared: { id: b.id, number: b.number } });

describe('Shared steps list', () => {
  it('lists the active shared steps with their usage, searches by number and name, and shows the archive', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop/shared-steps');
    const cards = await screen.findAllByTestId('shared-card');
    expect(cards).toHaveLength(2);
    expect(within(cards[0] as HTMLElement).getByText('SS-1')).toBeInTheDocument();
    expect(within(cards[0] as HTMLElement).getByText('Log in as a customer')).toBeInTheDocument();
    expect(within(cards[0] as HTMLElement).getByText('3 cases')).toBeInTheDocument();
    await user.type(screen.getByTestId('shared-search'), 'SS-2');
    await waitFor(() => expect(screen.getAllByTestId('shared-card')).toHaveLength(1));
    expect(screen.getByText('Add an item to the cart')).toBeInTheDocument();
    await user.clear(screen.getByTestId('shared-search'));
    await user.click(screen.getByRole('switch', { name: 'Archived' }));
    expect(await screen.findByText('Legacy login (SSO off)')).toBeInTheDocument();
    expect(screen.getAllByTestId('shared-card')).toHaveLength(1);
  });

  it('creates a shared step and opens it', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop/shared-steps');
    await user.click(await screen.findByTestId('new-shared-step'));
    const create = await screen.findByTestId('shared-create');
    expect(create).toBeDisabled();
    await user.type(screen.getByTestId('shared-name'), 'Check out');
    await user.type(screen.getByTestId('step-action'), 'Press **Pay**');
    await user.click(create);
    expect(await screen.findByTestId('shared-title')).toHaveTextContent('SS-4 Check out');
    expect(await screen.findAllByTestId('shared-step-row')).toHaveLength(1);
  });
});

describe('Shared step page', () => {
  it('shows the steps with their nesting, the versions with a diff, and the cases that use it', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop/shared-steps/1');
    expect(await screen.findByTestId('shared-title')).toHaveTextContent('SS-1 Log in as a customer');
    expect(screen.getByTestId('shared-usage-count')).toHaveTextContent('3 cases / 1 open runs');
    const rows = await screen.findAllByTestId('shared-step-row');
    expect(rows.map((r) => r.querySelector('[role=cell]')?.textContent)).toEqual(['1', '2', '2.1', '2.2']);
    await user.click(screen.getByRole('tab', { name: /Versions/ }));
    expect(await screen.findAllByTestId('shared-version-row')).toHaveLength(2);
    expect(await screen.findByTestId('version-diff')).toBeInTheDocument();
    await user.click(screen.getByRole('tab', { name: /Used by/ }));
    expect(await screen.findAllByTestId('shared-usage')).toHaveLength(3);
    expect(screen.getByTestId('shared-usages')).toHaveTextContent('1 open run pins a version of it');
  });

  it('edits into a new version and restores an older one', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop/shared-steps/1');
    await user.click(await screen.findByTestId('shared-edit'));
    const name = await screen.findByTestId('shared-name');
    await user.clear(name);
    await user.type(name, 'Sign in as a customer');
    await user.click(screen.getByTestId('shared-save'));
    await waitFor(() => expect(screen.getByTestId('shared-title')).toHaveTextContent('SS-1 Sign in as a customer'));
    expect(screen.getByText('v3')).toBeInTheDocument();
    await user.click(screen.getByRole('tab', { name: /Versions/ }));
    const rows = await screen.findAllByTestId('shared-version-row');
    expect(rows).toHaveLength(3);
    const v1 = rows.find((r) => r.dataset.version === '1');
    if (!v1) throw new Error('version 1 missing');
    await user.click(within(v1).getByTestId('shared-version-restore'));
    await user.click(await screen.findByTestId('shared-version-restore-confirm'));
    await waitFor(() => expect(screen.getAllByTestId('shared-version-row')).toHaveLength(4));
    expect(screen.getByTestId('shared-title')).toHaveTextContent('SS-1 Log in as a customer');
  });

  it('says so when somebody else saved first', async () => {
    const user = userEvent.setup();
    server.use(http.put('/api/v1/projects/:key/shared-steps/:n', () => HttpResponse.json({ code: 'precondition_failed', message: 'the resource changed', details: { currentRevision: 9, changedBy: 'anna', source: 'ui' } }, { status: 412 })));
    renderApp('/p/web-shop/shared-steps/1');
    await user.click(await screen.findByTestId('shared-edit'));
    await user.click(await screen.findByTestId('shared-save'));
    expect(await screen.findByTestId('shared-conflict')).toBeInTheDocument();
  });

  it('archives, inlines and refuses to delete a shared step that is used', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop/shared-steps/2');
    await user.click(await screen.findByTestId('shared-more'));
    await user.click(await screen.findByTestId('shared-purge'));
    await user.click(await screen.findByTestId('shared-purge-confirm'));
    await waitFor(() => expect(screen.queryByTestId('shared-purge-confirm')).toBeNull());
    expect(screen.getByTestId('shared-title')).toHaveTextContent('SS-2');
    expect(screen.getByTestId('shared-usage-count')).toHaveTextContent('1 cases');
    await user.click(await screen.findByTestId('shared-more'));
    await user.click(await screen.findByTestId('shared-inline'));
    await user.click(await screen.findByTestId('shared-inline-confirm'));
    await waitFor(() => expect(screen.getByTestId('shared-usage-count')).toHaveTextContent('0 cases / 0 open runs'));
    await user.click(await screen.findByTestId('shared-more'));
    await user.click(await screen.findByTestId('shared-archive'));
    await waitFor(() => expect(screen.getByText('archived')).toBeInTheDocument());
  });
});

describe('Inserting a shared step into a case', () => {
  it('Ctrl+Shift+S opens the picker, the search finds SS- numbers and the pick becomes a reference step', async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    renderWith(<StepsEditor project="web-shop" caseNumber={1} steps={[step('s_1', 'first')]} onChange={onChange} onAnnounce={() => undefined} />);
    const field = screen.getByTestId('step-action');
    await user.click(field);
    fireEvent.keyDown(field, { key: 'S', ctrlKey: true, shiftKey: true });
    const search = await screen.findByTestId('shared-pick-search');
    await user.type(search, 'SS-2');
    const row = await screen.findByTestId('shared-pick');
    expect(row).toHaveTextContent('Add an item to the cart');
    await user.click(row);
    await waitFor(() => expect(onChange).toHaveBeenCalled());
    const steps = onChange.mock.calls[0]?.[0] as CaseStep[];
    expect(steps).toHaveLength(2);
    expect(steps[1]?.shared).toMatchObject({ number: 2 });
    expect(steps[1]?.action).toBe('');
  });

  it('shows a referenced step as a block with its number and name, and takes it out again', async () => {
    const user = userEvent.setup();
    const b = brief();
    const onChange = vi.fn();
    renderWith(<StepsEditor project="web-shop" caseNumber={1} steps={[step('s_1', 'first'), ref(b)]} briefs={[b]} onChange={onChange} onAnnounce={() => undefined} />);
    const block = screen.getByTestId('step-shared');
    expect(block).toHaveTextContent('SS-1');
    expect(block).toHaveTextContent('Log in as a customer');
    expect(block).toHaveTextContent('v2');
    await user.click(within(block).getByRole('button', { name: /Show the steps of SS-1/ }));
    expect(within(block).getByTestId('step-shared-preview')).toHaveTextContent('2. Sign in');
    await user.click(within(block).getByRole('button', { name: 'Delete step 2' }));
    expect(onChange).toHaveBeenCalledWith([expect.objectContaining({ id: 's_1' })]);
  });

  it('a shared step itself cannot take shared steps or attachments and nests only two levels', () => {
    renderWith(<StepsEditor project="web-shop" caseNumber={null} steps={[step('s_1', 'a', [step('s_2', 'b')])]} onChange={() => undefined} onAnnounce={() => undefined} allowShared={false} maxDepth={2} attachments={false} />);
    expect(screen.queryByTestId('step-insert-shared')).toBeNull();
    expect(screen.queryByRole('button', { name: /Attach a file/ })).toBeNull();
    expect(screen.getByRole('button', { name: 'Nest step 1.1' })).toBeDisabled();
  });
});

async function oneCase(): Promise<Case> {
  const n = [...tmState().cases.keys()][0] ?? 1;
  return (await getVersioned<Case>(`/api/v1/projects/web-shop/cases/${String(n)}`)).value;
}

describe('A case scenario with shared steps', () => {
  it('shows a collapsed block per reference and expands it with its steps', async () => {
    const user = userEvent.setup();
    const b = brief();
    const c = await oneCase();
    const own = step('s_own', 'Own step');
    const shown: Case = { ...c, automated: false, body: { ...c.body, steps: [own, ref(b)] }, html: { ...c.html, steps: [{ id: 's_own', action: '<p>Own step</p>', expected: '', data: '', steps: [] }] }, sharedSteps: [b] };
    renderWith(<ScenarioView project="web-shop" c={shown} />);
    const block = screen.getByTestId('step-shared-view');
    expect(block).toHaveTextContent('SS-1');
    expect(block).toHaveTextContent('Log in as a customer');
    expect(block).toHaveTextContent('v2 · 2 steps');
    expect(screen.getAllByTestId('step-view')).toHaveLength(1);
    await user.click(within(block).getByTestId('shared-toggle'));
    const rows = screen
      .getAllByRole('row')
      .slice(1)
      .map((r) => r.querySelector('[role=cell]')?.textContent);
    expect(rows).toEqual(['1', '2', '2.1', '2.2', '2.2.1']);
  });

  it('extracts adjacent steps only, and then asks for a name', async () => {
    const user = userEvent.setup();
    const c = await oneCase();
    const shown: Case = { ...c, automated: false, archivedAt: null, body: { ...c.body, steps: [step('s_a', 'a'), step('s_b', 'b'), step('s_c', 'c')] }, html: { ...c.html, steps: ['s_a', 's_b', 's_c'].map((id) => ({ id, action: `<p>${id}</p>`, expected: '', data: '', steps: [] })) }, sharedSteps: [] };
    const mock = tmState().cases.get(shown.number);
    if (!mock) throw new Error('no mock case');
    mock.body.steps = shown.body.steps;
    renderWith(<ScenarioView project="web-shop" c={{ ...shown, revision: mock.revision }} />);
    await user.click(await screen.findByTestId('extract-toggle'));
    const boxes = screen.getAllByTestId('extract-pick');
    await user.click(boxes[0] as HTMLElement);
    await user.click(boxes[2] as HTMLElement);
    expect(screen.getByTestId('extract-start')).toBeDisabled();
    expect(screen.getByText('The steps must be adjacent and not shared')).toBeInTheDocument();
    await user.click(boxes[1] as HTMLElement);
    expect(screen.getByTestId('extract-start')).toBeEnabled();
    await user.click(screen.getByTestId('extract-start'));
    const name = await screen.findByTestId('extract-name');
    await user.type(name, 'All three');
    await user.click(screen.getByTestId('extract-confirm'));
    await waitFor(() => expect(tmState().cases.get(shown.number)?.body.steps.some((s) => s.shared)).toBe(true));
  });
});

async function oneItem(): Promise<RunItem> {
  const id = tmState().runItems[0]?.id ?? '';
  return (await getVersioned<RunItem>(`/api/v1/projects/web-shop/run-items/${id}`)).value;
}

describe('The manual runner with shared steps', () => {
  const props = () => ({
    fields: { readOnly: false, onText: () => undefined, onFiles: () => undefined, actualRef: createRef<HTMLTextAreaElement>(), commentRef: createRef<HTMLTextAreaElement>(), fileRef: createRef<HTMLInputElement>(), uploading: false },
    effective: new Map(),
    local: new Map(),
  });

  it('collapses a "SS-1 v2" block and opens it again when a step inside becomes the current one', async () => {
    const user = userEvent.setup();
    const item = await oneItem();
    const b = brief();
    const expanded: CaseStep = { ...ref(b), shared: { id: b.id, number: 1, version: 2 }, steps: b.steps };
    const shown: RunItem = {
      ...item,
      steps: [step('s_x', 'Before'), expanded],
      stepsHtml: [
        { id: 's_x', action: '<p>Before</p>', expected: '', data: '', steps: [] },
        { id: 's_ref', action: '', expected: '', data: '', steps: b.stepsHtml },
      ],
      sharedPins: [{ id: b.id, number: 1, name: b.name, version: 2, current: 3 }],
    };
    const flat = [
      { path: '1', step: shown.steps[0] as CaseStep, depth: 0, parent: null, hasChildren: false },
      { path: '2', step: expanded, depth: 0, parent: null, hasChildren: true },
      { path: '2.1', step: b.steps[0] as CaseStep, depth: 1, parent: '2', hasChildren: false },
      { path: '2.2', step: b.steps[1] as CaseStep, depth: 1, parent: '2', hasChildren: true },
      { path: '2.2.1', step: (b.steps[1] as CaseStep).steps[0] as CaseStep, depth: 2, parent: '2.2', hasChildren: false },
    ];
    const p = props();
    function Harness() {
      const [cur, setCur] = useState(0);
      return (
        <>
          <button onClick={() => setCur(3)}>go to 2.2</button>
          <RunSteps project="web-shop" item={shown} flat={flat} current={cur} onCurrent={setCur} onSet={() => undefined} {...p} />
        </>
      );
    }
    renderWith(<Harness />);
    const head = screen.getByTestId('run-shared-head');
    expect(head).toHaveTextContent('SS-1 v2');
    expect(head).toHaveTextContent('Log in as a customer');
    expect(head).toHaveTextContent('v3 available');
    expect(screen.getAllByTestId('run-step').filter((li) => !li.hidden)).toHaveLength(5);
    await user.click(screen.getByTestId('run-shared-toggle'));
    expect(screen.getAllByTestId('run-step').filter((li) => !li.hidden).map((li) => li.dataset.path)).toEqual(['1', '2']);
    await user.click(screen.getByTestId('run-shared-toggle'));
    expect(screen.getAllByTestId('run-step').filter((li) => !li.hidden)).toHaveLength(5);
    await user.click(screen.getByTestId('run-shared-toggle'));
    expect(screen.getAllByTestId('run-step').filter((li) => !li.hidden)).toHaveLength(2);
    await user.click(screen.getByText('go to 2.2'));
    await waitFor(() => expect(screen.getAllByTestId('run-step').filter((li) => !li.hidden)).toHaveLength(5));
  });

  it('offers "Update to current version" only for an untouched item with an older pin', async () => {
    const user = userEvent.setup();
    const item = await oneItem();
    const pins = [{ id: '0190a1b2-c3d4-7e5f-8a9b-000000000001', number: 1, name: 'Log in', version: 1, current: 2 }];
    let refreshed = 0;
    server.use(
      http.get('/api/v1/projects/web-shop/run-items/:id', () => HttpResponse.json({ ...item, status: 'untested', stepResults: [], sharedPins: pins }, { headers: { ETag: `"r${String(item.revision)}"` } })),
      http.post('/api/v1/projects/web-shop/run-items/:id/refresh', () => {
        refreshed += 1;
        return HttpResponse.json({ ...item, status: 'untested', stepResults: [], sharedPins: [{ ...pins[0], version: 2 }] }, { headers: { ETag: `"r${String(item.revision + 1)}"` } });
      }),
    );
    renderApp(`/p/web-shop/launches/${item.launchId}/run/${item.id}`);
    const banner = await screen.findByTestId('shared-refresh');
    expect(banner).toHaveTextContent('SS-1 v1 → v2');
    await user.click(screen.getByTestId('shared-refresh-button'));
    await waitFor(() => expect(refreshed).toBe(1));
    await waitFor(() => expect(screen.queryByTestId('shared-refresh')).toBeNull());
  });

  it('shows no banner once the item has results', async () => {
    const item = await oneItem();
    const pins = [{ id: '0190a1b2-c3d4-7e5f-8a9b-000000000001', number: 1, name: 'Log in', version: 1, current: 2 }];
    server.use(http.get('/api/v1/projects/web-shop/run-items/:id', () => HttpResponse.json({ ...item, status: 'in_progress', sharedPins: pins }, { headers: { ETag: `"r${String(item.revision)}"` } })));
    renderApp(`/p/web-shop/launches/${item.launchId}/run/${item.id}`);
    await screen.findByTestId('run-steps');
    await new Promise((r) => setTimeout(r, 50));
    expect(screen.queryByTestId('shared-refresh')).toBeNull();
  });
});
