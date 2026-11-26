import { describe, expect, it } from 'vitest';
import { generatorValues, newBudget, newRowId, SUBSTITUTION_BUDGET, parseTsv, placeholders, pruneValues, substitute, substituteHtml, tableProblems, unknownPlaceholders, valuesLabel } from './parameters';
import { deriveStatus } from '../../mocks/parameters';

describe('placeholders', () => {
  it('finds ${name} and leaves everything else as text', () => {
    const t = 'Log in as ${user} on ${ browser } with $${x} and ${1a} ${user}${';
    expect(placeholders(t).map((p) => p.name)).toEqual(['user', 'x', 'user']);
    expect(substitute(t, { user: 'ann' })).toBe('Log in as ann on ${ browser } with $${x} and ${1a} ann${');
    expect(substitute('${a}${b}', { a: '1' })).toBe('1${b}');
    expect(substitute('${a}', { a: '$&' })).toBe('$&');
  });

  it('replaces values in the text of HTML only, never in attributes or as markup', () => {
    const html = '<p>Pay in <strong>${currency}</strong> <a href="${url}">${url}</a></p>';
    const out = substituteHtml(html, { currency: 'EUR', url: 'javascript:alert(1)' });
    expect(out).toContain('<strong>EUR</strong>');
    expect(out).toContain('href="${url}"');
    expect(out).toContain('>javascript:alert(1)</a>');
    const evil = substituteHtml('<p>${v}</p>', { v: '<img src=x onerror=alert(1)>' });
    expect(evil).toBe('<p>&lt;img src=x onerror=alert(1)&gt;</p>');
  });

  it('lists placeholders without a column with their paths', () => {
    const steps = [{ id: 's_1', action: 'Pay in ${currency}', expected: '', data: '${card}', attachments: [], steps: [{ id: 's_2', action: '${user}', expected: '', data: '', attachments: [], steps: [] }] }];
    expect(unknownPlaceholders('As ${user}', steps, ['currency'])).toEqual([
      { path: 'body.preconditions', name: 'user' },
      { path: 'body.steps[0].data', name: 'card' },
      { path: 'body.steps[0].steps[0].action', name: 'user' },
    ]);
  });
});

describe('the substitution budget (as on the server: 1 MiB of replacements per view)', () => {
  it('counts UTF-8 bytes and leaves the placeholders that no longer fit', () => {
    const b = newBudget(5);
    expect(substitute('${a}-${a}-${a}', { a: 'xx' }, b)).toBe('xx-xx-${a}');
    expect(b.left).toBe(1);
    const wide = newBudget(5);
    expect(substitute('${a}', { a: 'ééé' }, wide)).toBe('${a}');
    expect(wide.left).toBe(5);
    expect(substitute('${a}${a}', { a: 'é' }, wide)).toBe('éé');
    expect(wide.left).toBe(1);
  });

  it('stops a shared step text full of placeholders from growing without a limit', () => {
    const text = '${a}'.repeat(4096);
    const value = 'v'.repeat(1024);
    const unbounded = substitute(text, { a: value });
    expect(unbounded.length).toBe(4 * 1024 * 1024);
    const budget = newBudget();
    const bounded = substitute(text, { a: value }, budget);
    expect(bounded.length).toBeLessThanOrEqual(SUBSTITUTION_BUDGET + text.length);
    expect(budget.left).toBeLessThan(1024);
    expect(bounded.endsWith('${a}')).toBe(true);
  });

  it('shares the budget between the texts of an HTML view, text nodes only', () => {
    const budget = newBudget(6);
    const out = [substituteHtml('<p>${v}</p>', { v: 'abcd' }, budget), substituteHtml('<p>${v}</p>', { v: 'abcd' }, budget)];
    expect(out).toEqual(['<p>abcd</p>', '<p>${v}</p>']);
  });
});

describe('the parameters table', () => {
  it('makes ULID row ids', () => {
    const id = newRowId(1_700_000_000_000);
    expect(id).toMatch(/^r_[0-9A-HJKMNP-TV-Z]{26}$/);
    expect(newRowId()).not.toBe(newRowId());
  });

  it('checks columns, sizes and repeated enabled rows', () => {
    const p = {
      columns: ['browser', 'browser', '1x'],
      rows: [
        { id: 'r_1', values: { browser: 'a' }, enabled: true },
        { id: 'r_2', values: { browser: 'a' }, enabled: true },
        { id: 'r_3', values: { browser: 'a' }, enabled: false },
        { id: 'r_4', values: { browser: 'x'.repeat(1025) }, enabled: true },
      ],
    };
    const problems = tableProblems(p);
    expect(problems.some((x) => x.includes('appears twice'))).toBe(true);
    expect(problems.some((x) => x.includes('“1x” is not a valid'))).toBe(true);
    expect(problems.some((x) => x.includes('Rows 1 and 2'))).toBe(true);
    expect(problems.some((x) => x.includes('Rows 1 and 3'))).toBe(false);
    expect(problems.some((x) => x.includes('Row 4'))).toBe(true);
    expect(tableProblems({ columns: ['a'], rows: [{ id: 'r_1', values: { a: '1' }, enabled: true }] })).toEqual([]);
  });

  it('pastes tab-separated rows with or without a header', () => {
    const withHeader = parseTsv('browser\tcurrency\nfirefox\tEUR\r\n\nchromium\tUSD\n', ['browser'], true);
    expect(withHeader.columns).toEqual(['browser', 'currency']);
    expect(withHeader.rows.map((r) => r.values)).toEqual([
      { browser: 'firefox', currency: 'EUR' },
      { browser: 'chromium', currency: 'USD' },
    ]);
    const plain = parseTsv('a\tb\tc', ['x', 'y'], false);
    expect(plain.rows[0]?.values).toEqual({ x: 'a', y: 'b' });
    expect(plain.rows[0]?.enabled).toBe(true);
  });

  it('prunes values of removed columns and reads generator input', () => {
    const p = pruneValues({ columns: ['a'], rows: [{ id: 'r_1', values: { a: '1', b: '2' }, enabled: true }] });
    expect(p.rows[0]?.values).toEqual({ a: '1' });
    expect(generatorValues({ a: 'x\n\n y \nx', b: '  ' })).toEqual({ a: ['x', 'y'] });
    expect(valuesLabel(['b', 'a'], { a: '1', b: '2' })).toBe('b=2 · a=1');
  });

  it('derives the item status from its iterations as the server does', () => {
    expect(deriveStatus(['untested', 'untested'])).toBe('untested');
    expect(deriveStatus(['passed', 'untested'])).toBe('in_progress');
    expect(deriveStatus(['passed', 'skipped'])).toBe('passed');
    expect(deriveStatus(['skipped', 'skipped'])).toBe('skipped');
    expect(deriveStatus(['blocked', 'passed'])).toBe('blocked');
    expect(deriveStatus(['blocked', 'failed', 'untested'])).toBe('failed');
  });
});
