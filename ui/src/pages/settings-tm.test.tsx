import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';
import { server } from '../mocks/node';
import { renderApp } from '../test/render';

describe('project settings and test management', () => {
  it('saving the general settings keeps retention.manualDays and caseSync', async () => {
    const caseSync = { enabled: true, branches: ['main'], useCiDefaultBranch: true, includeDebug: false, createCases: true, folderFrom: 'suite', rootFolder: 'Automated', staleAfterDays: 30, matchTmsLinks: false };
    let sent: Record<string, unknown> | null = null;
    server.use(
      http.get('/api/v1/projects/web-shop/settings', () =>
        HttpResponse.json({ flaky: { historyDepth: 10, stabilizationPeriod: 5, includePassedTests: false }, retention: { rawDays: 90, keepImportant: true, manualDays: 365 }, rollup: { afterDays: 30 }, gates: [], notifications: [], issueTrackers: [], vcs: null, fastmock: null, caseSync }),
      ),
      http.put('/api/v1/projects/web-shop/settings', async ({ request }) => {
        sent = (await request.json()) as Record<string, unknown>;
        return HttpResponse.json(sent);
      }),
    );
    const user = userEvent.setup();
    renderApp('/p/web-shop/settings');
    const save = await screen.findByTestId('settings-save');
    await user.click(save);
    await waitFor(() => expect(sent).not.toBeNull());
    expect(sent).toMatchObject({ retention: { rawDays: 90, keepImportant: true, manualDays: 365 }, caseSync });
  });

  it('sets the retention of test case versions', async () => {
    const sent: { body: { retention?: Record<string, unknown> } | null } = { body: null };
    server.use(
      http.get('/api/v1/projects/web-shop/settings', () =>
        HttpResponse.json({ flaky: { historyDepth: 10, stabilizationPeriod: 5, includePassedTests: false }, retention: { rawDays: 90, keepImportant: true, caseVersionsDays: 365 }, rollup: { afterDays: 30 }, gates: [], notifications: [], issueTrackers: [], vcs: null, fastmock: null }),
      ),
      http.put('/api/v1/projects/web-shop/settings', async ({ request }) => {
        sent.body = (await request.json()) as { retention?: Record<string, unknown> };
        return HttpResponse.json(sent.body);
      }),
    );
    const user = userEvent.setup();
    renderApp('/p/web-shop/settings');
    const keep = await screen.findByTestId('settings-case-versions-keep');
    expect(screen.getByTestId('settings-case-versions-days')).toHaveValue('365');
    await user.type(keep, '20');
    await user.click(screen.getByTestId('settings-save'));
    await waitFor(() => expect(sent.body).not.toBeNull());
    expect(sent.body?.retention).toMatchObject({ caseVersionsKeep: 20, caseVersionsDays: 365, rawDays: 90 });
  });
});
