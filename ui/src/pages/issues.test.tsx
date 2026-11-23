import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { CreateIssueModal } from '../components/CreateIssueModal';
import { apiSaved, cleanApi, newRequestId, trackerApiErrors } from '../lib/trackers';
import { renderApp, renderWith } from '../test/render';

describe('issues in trackers', () => {
  it('lists followed issues with their state and suggests unmuting after a close', async () => {
    renderApp('/p/web-shop/issues');
    const rows = await screen.findAllByTestId('issue-row');
    expect(rows).toHaveLength(3);
    expect(within(rows[1] as HTMLElement).getByTestId('issue-state')).toHaveTextContent('In Progress');
    const suggestion = await screen.findByTestId('unmute-suggestion');
    expect(suggestion).toHaveTextContent('SHOP-1180');
    await userEvent.click(within(suggestion).getByTestId('unmute-linked'));
    await waitFor(() => expect(screen.queryByTestId('unmute-suggestion')).not.toBeInTheDocument());
  });

  it('previews and creates an issue, then links back to it', async () => {
    renderWith(<CreateIssueModal project="web-shop" target={{ label: 'shop.Checkout#pays', count: 1, resultIds: ['r-1'] }} onClose={() => undefined} />);
    expect(await screen.findByText(/Jira SHOP \(jira\)/)).toBeInTheDocument();
    await userEvent.type(screen.getByTestId('issue-note'), 'Seen after the deploy');
    await userEvent.click(screen.getByTestId('issue-preview-toggle'));
    await waitFor(() => expect(screen.getByTestId('issue-preview')).toHaveTextContent('Seen after the deploy'), { timeout: 5000 });
    await userEvent.click(screen.getByText(/As sent/));
    expect(screen.getByTestId('issue-preview-body')).toHaveTextContent('Seen after the deploy');
    await userEvent.click(screen.getByTestId('issue-create'));
    const link = await screen.findByTestId('issue-created-link');
    expect(link).toHaveAttribute('href', expect.stringMatching(/^https:\/\/issues\.example\.com\/browse\/SHOP-\d+$/));
    expect(screen.getByTestId('issue-created')).toHaveTextContent('1 screenshot attached');
  });

  it('shows the API connection of a tracker in the settings', async () => {
    renderApp('/p/web-shop/settings?tab=integrations');
    const api = await screen.findAllByTestId('tracker-api');
    expect(within(api[0] as HTMLElement).getByTestId('tracker-api-enabled')).toBeChecked();
    expect(within(api[0] as HTMLElement).getByTestId('tracker-api-project')).toHaveValue('SHOP');
    expect(within(api[1] as HTMLElement).getByTestId('tracker-api-enabled')).not.toBeChecked();
  });

  it('validates and cleans tracker API settings', () => {
    const t = {
      type: 'jira' as const,
      id: 'jira',
      urlTemplate: 'https://x.example.com/browse/{key}',
      keyRegex: '.+',
      api: {
        url: 'https://x.example.com',
        tokenSecretRef: 't',
        project: 'shop',
        sync: 'webhook',
        labels: [],
        deployment: 'cloud',
      },
    };
    expect(Object.keys(trackerApiErrors(t)).sort()).toEqual(['project', 'webhook']);
    const clean = cleanApi({
      ...t,
      api: { ...t.api, project: 'SHOP', sync: 'poll' },
    });
    expect(clean.api).toEqual({
      url: 'https://x.example.com',
      tokenSecretRef: 't',
      project: 'SHOP',
    });
    expect(
      apiSaved(
        [
          {
            ...t,
            api: {
              project: 'shop',
              url: 'https://x.example.com',
              tokenSecretRef: 't',
              sync: 'webhook',
              labels: [],
              deployment: 'cloud',
            },
          },
        ],
        t,
      ),
    ).toBe(true);
    expect(apiSaved([t], { ...t, api: { ...t.api, project: 'SHOP' } })).toBe(false);
    expect(newRequestId()).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  });
});
