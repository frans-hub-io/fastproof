import { afterEach, describe, expect, it } from 'vitest';
import { installPrintLight } from './printLight';

const ATTR = 'data-mantine-color-scheme';

afterEach(() => document.documentElement.removeAttribute(ATTR));

describe('installPrintLight', () => {
  it('switches to the light theme for printing and restores the chosen one afterwards', () => {
    document.documentElement.setAttribute(ATTR, 'dark');
    const stop = installPrintLight();
    window.dispatchEvent(new Event('beforeprint'));
    expect(document.documentElement.getAttribute(ATTR)).toBe('light');
    window.dispatchEvent(new Event('beforeprint'));
    window.dispatchEvent(new Event('afterprint'));
    expect(document.documentElement.getAttribute(ATTR)).toBe('dark');
    stop();
  });

  it('stops listening and restores on cleanup', () => {
    document.documentElement.setAttribute(ATTR, 'dark');
    const stop = installPrintLight();
    window.dispatchEvent(new Event('beforeprint'));
    stop();
    expect(document.documentElement.getAttribute(ATTR)).toBe('dark');
    window.dispatchEvent(new Event('beforeprint'));
    expect(document.documentElement.getAttribute(ATTR)).toBe('dark');
  });
});
