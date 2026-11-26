import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';
import { server } from '../../mocks/node';
import { resetRequirements } from '../../mocks/requirements';
import { renderApp, renderWith } from '../../test/render';
import { CoverageChip, CountsBar } from './coverage';
import { COVERAGE_ORDER } from './coverageStyle';
import { RequirementsTab } from '../cases/RequirementsTab';

beforeEach(() => {
  resetRequirements();
});

describe('coverage widgets', () => {
  it('labels every coverage status with text, not only colour', () => {
    for (const s of COVERAGE_ORDER) {
      const { unmount } = renderWith(<CoverageChip status={s} />);
      expect(document.querySelector(`[data-coverage="${s}"]`)?.textContent.length).toBeGreaterThan(3);
      unmount();
    }
  });

  it('describes the counts bar for screen readers', () => {
    renderWith(<CountsBar counts={{ total: 4, passed: 2, failed: 1, notRun: 1, skipped: 0 }} />);
    expect(screen.getByRole('img')).toHaveAccessibleName('4 cases: 2 passed, 1 failed, 1 not run');
  });
});

describe('Requirements page', () => {
  it('shows the top level and loads children when a row expands', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop/requirements');
    expect(await screen.findAllByTestId('requirement-row')).toHaveLength(4);
    await user.click(screen.getByRole('button', { name: 'Expand REQ-1' }));
    await waitFor(() => expect(screen.getAllByTestId('requirement-row')).toHaveLength(7));
    expect(screen.getByText('Search finds products')).toBeInTheDocument();
  });

  it('hides the write actions from a reader', async () => {
    window.localStorage.setItem('fastproof-mock-role', 'viewer');
    try {
      renderApp('/p/web-shop/requirements');
      await screen.findAllByTestId('requirement-row');
      expect(screen.queryByTestId('new-requirement')).not.toBeInTheDocument();
      expect(screen.queryByTestId('requirements-import')).not.toBeInTheDocument();
    } finally {
      window.localStorage.removeItem('fastproof-mock-role');
    }
  });

  it('shows the message of an invalid query under the input', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop/requirements');
    await screen.findAllByTestId('requirement-row');
    await user.type(screen.getByTestId('requirements-query'), 'colour = red');
    expect(await screen.findByText(/unknown attribute "colour"/)).toBeInTheDocument();
  });

  it('shows an error row when the tree cannot load', async () => {
    server.use(http.get('/api/v1/projects/:key/requirements/tree', () => HttpResponse.json({ code: 'internal', message: 'the store is down' }, { status: 500 })));
    renderApp('/p/web-shop/requirements');
    expect(await screen.findByText('the store is down')).toBeInTheDocument();
  });
});

describe('Requirement page', () => {
  it('shows the path, the linked cases and the proposal, and confirms it', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop/requirements/3');
    expect(await screen.findByTestId('requirement-heading')).toHaveTextContent('REQ-3 Search tolerates typos');
    const rows = await screen.findAllByTestId('linked-case-row');
    expect(rows).toHaveLength(2);
    expect(screen.getByTestId('proposed-badge')).toBeInTheDocument();
    await user.click(screen.getByTestId('confirm-link'));
    await waitFor(() => expect(screen.queryByTestId('proposed-badge')).not.toBeInTheDocument());
  });

  it('says so when the requirement does not exist', async () => {
    renderApp('/p/web-shop/requirements/999');
    expect(await screen.findByText('Requirement not found')).toBeInTheDocument();
  });
});

describe('Coverage page', () => {
  it('lists coverage per top-level requirement and drills down', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop/coverage');
    const rows = await screen.findAllByTestId('coverage-row');
    expect(rows).toHaveLength(4);
    expect(within(rows[3] as HTMLElement).getByText('Uncovered')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Expand REQ-6' }));
    await waitFor(() => expect(screen.getAllByTestId('coverage-row')).toHaveLength(6));
  });

  it('asks for a plan before it counts one', async () => {
    renderApp('/p/web-shop/coverage?scope=plan');
    expect(await screen.findByText('Choose what to count, then press Apply.')).toBeInTheDocument();
    expect(screen.queryByTestId('coverage-table')).not.toBeInTheDocument();
  });
});

describe('Case requirements tab', () => {
  it('says when a case covers nothing', async () => {
    renderWith(<RequirementsTab projectKey="web-shop" caseNumber={30} />);
    expect(await screen.findByTestId('requirements-tab-empty')).toBeInTheDocument();
  });

  it('rejects text that is not a requirement number', async () => {
    const user = userEvent.setup();
    renderWith(<RequirementsTab projectKey="web-shop" caseNumber={30} />);
    await screen.findByTestId('requirements-tab-empty');
    await user.type(screen.getByTestId('link-requirement-input'), 'nonsense');
    expect(screen.getByTestId('link-requirement-submit')).toBeDisabled();
    expect(screen.getByText('Enter a requirement number')).toBeInTheDocument();
  });
});
