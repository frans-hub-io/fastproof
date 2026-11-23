import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Launch } from '../../api/types';
import { buildExport, exportFileName, exportFilters } from '../../lib/launchExport';
import { launchDef, RICH_LAUNCH_ID } from '../../mocks/fixtures';
import { Providers } from '../../test/render';
import { ExportMenu } from './ExportMenu';

function rich(): Launch {
  const def = launchDef(RICH_LAUNCH_ID);
  if (!def) throw new Error('fixture');
  return def.launch;
}

describe('launch export', () => {
  const created: Blob[] = [];
  beforeEach(() => {
    created.length = 0;
    URL.createObjectURL = vi.fn((b: Blob | MediaSource) => {
      created.push(b as Blob);
      return 'blob:test';
    });
    URL.revokeObjectURL = vi.fn();
  });
  afterEach(() => vi.restoreAllMocks());

  it('keeps only the filters the result list understands', () => {
    const sp = new URLSearchParams('tab=results&status=failed&q=pay&label=x:y&test=abc&env=prod&junk=1');
    expect(exportFilters(sp)).toEqual({ status: 'failed', q: 'pay', label: 'x:y', env: 'prod' });
  });

  it('names the file after the project and the number', () => {
    expect(exportFileName('web-shop', rich(), 'pdf')).toBe(`launch-web-shop-${String(rich().number)}.html`);
    expect(exportFileName('web-shop', rich(), 'xlsx')).toMatch(/\.xlsx$/);
  });

  it('exports a small launch inline with the filters', async () => {
    const blob = await buildExport('web-shop', rich(), 'html', { status: 'failed' });
    const text = await blob.text();
    expect(text).toContain('format=html');
    expect(text).toContain('status=failed');
  });

  it('exports a large launch with a background job and reports progress', async () => {
    const big = { ...rich(), counts: { ...rich().counts, total: 20_000 } } as Launch;
    const seen: string[] = [];
    const blob = await buildExport('web-shop', big, 'xlsx', {}, (j) => seen.push(j.status), 1);
    expect(await blob.text()).toContain('job');
    expect(seen[0]).toBe('queued');
    expect(seen.at(-1)).toBe('done');
  });

  it('saves the HTML report from the menu', async () => {
    const user = userEvent.setup();
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => undefined);
    render(
      <Providers route="/p/web-shop/launches/x?status=failed">
        <ExportMenu project="web-shop" launch={rich()} />
      </Providers>,
    );
    await user.click(screen.getByTestId('export-menu'));
    expect(await screen.findByText('Results with the current filters')).toBeInTheDocument();
    await user.click(screen.getByTestId('export-html'));
    await waitFor(() => expect(click).toHaveBeenCalled());
    expect(created).toHaveLength(1);
  });

  it('prints the report for PDF', async () => {
    const user = userEvent.setup();
    const print = vi.fn();
    const win = { addEventListener: (_: string, cb: () => void) => cb(), focus: vi.fn(), print } as unknown as Window;
    vi.spyOn(window, 'open').mockReturnValue(win);
    render(
      <Providers route="/p/web-shop/launches/x">
        <ExportMenu project="web-shop" launch={rich()} />
      </Providers>,
    );
    await user.click(screen.getByTestId('export-menu'));
    await user.click(await screen.findByTestId('export-pdf'));
    await waitFor(() => expect(print).toHaveBeenCalled());
  });
});
