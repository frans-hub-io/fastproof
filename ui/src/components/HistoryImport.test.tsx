import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { planFiles } from '../api/historyImports';
import { renderWith } from '../test/render';
import { HistoryImport } from './HistoryImport';

function file(name: string, size = 10): File {
  return new File([new Uint8Array(size)], name, { type: 'application/json' });
}

function chooser(container: HTMLElement): HTMLInputElement {
  const input = container.querySelector<HTMLInputElement>('input[type="file"]');
  if (!input) throw new Error('no file input');
  return input;
}

describe('planFiles', () => {
  it('maps the files of an Allure 2 history directory to fields', () => {
    const plan = planFiles('allure2', [file('history.json'), file('retry-trend.json'), file('history-trend.json')]);
    if ('error' in plan) throw new Error(plan.error);
    expect([...plan.form.keys()].sort()).toEqual(['history', 'historyTrend', 'retryTrend']);
  });

  it('refuses unknown names, duplicates, empty and missing files', () => {
    expect(planFiles('allure2', [])).toEqual({ error: 'Choose the history files.' });
    expect(planFiles('allure2', [file('history.json'), file('other.json')])).toMatchObject({ error: expect.stringContaining('other.json') as string });
    expect(planFiles('allure2', [file('history.json'), file('history.json')])).toEqual({ error: 'history.json is chosen twice.' });
    expect(planFiles('allure2', [file('history.json', 0)])).toEqual({ error: 'history.json is empty.' });
    expect(planFiles('allure2', [file('retry-trend.json')])).toEqual({ error: 'history.json is required.' });
    expect(planFiles('allure2', [file('history.json'), file('retry-trend.json', 4 * 1024 * 1024 + 1)])).toEqual({ error: 'retry-trend.json is larger than 4 MiB.' });
  });

  it('takes one file of any name for Allure 3', () => {
    const plan = planFiles('allure3', [file('anything.jsonl')]);
    if ('error' in plan) throw new Error(plan.error);
    expect([...plan.form.keys()]).toEqual(['history']);
    expect(planFiles('allure3', [file('a.jsonl'), file('b.jsonl')])).toEqual({ error: 'An Allure 3 import takes one history file.' });
  });
});

describe('HistoryImport', () => {
  it('imports a history file and shows the counters and notes when it is done', async () => {
    const user = userEvent.setup();
    const { container } = renderWith(<HistoryImport project="web-shop" canWrite />);
    expect(screen.getByTestId('history-submit')).toBeDisabled();
    await user.upload(chooser(container), file('history.json'));
    await user.type(screen.getByTestId('history-branch'), 'main');
    await user.click(screen.getByTestId('history-submit'));
    const row = await screen.findByTestId('history-import-row');
    expect(row).toHaveTextContent('main');
    await waitFor(() => expect(screen.getByTestId('history-import-status')).toHaveTextContent('done'), { timeout: 8_000 });
    expect(screen.getByTestId('history-import-row')).toHaveTextContent('3 of 3 reports');
    expect(screen.getByTestId('history-import-warnings')).toHaveTextContent('does not know');
  }, 20_000);

  it('shows the error of a failed import', async () => {
    const user = userEvent.setup();
    const { container } = renderWith(<HistoryImport project="web-shop" canWrite />);
    await user.upload(chooser(container), file('history.json'));
    await user.type(screen.getByTestId('history-environment'), 'broken');
    await user.click(screen.getByTestId('history-submit'));
    await waitFor(() => expect(screen.getByTestId('history-import-error')).toHaveTextContent('not an Allure 2 history'), { timeout: 8_000 });
  }, 20_000);

  it('is read-only without the permission', () => {
    renderWith(<HistoryImport project="web-shop" canWrite={false} />);
    expect(screen.getByTestId('history-submit')).toBeDisabled();
    expect(screen.getByText('Importing history needs the permission to change project settings.')).toBeVisible();
  });
});
