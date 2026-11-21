import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { renderSafeHtml } from './safeHtml';

describe('renderSafeHtml', () => {
  it('keeps safe formatting and drops scripts, handlers and unsafe links', () => {
    const { container } = render(
      <div>{renderSafeHtml('<p onclick="x()">Hi <b>there</b><script>alert(1)</script></p><a href="javascript:alert(1)">bad</a><a href="https://example.com">ok</a><img src=x onerror=alert(1)>')}</div>,
    );
    expect(container.querySelector('script')).toBeNull();
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('[onclick]')).toBeNull();
    expect(container.querySelector('b')?.textContent).toBe('there');
    const links = container.querySelectorAll('a');
    expect(links).toHaveLength(1);
    expect(links[0]?.getAttribute('href')).toBe('https://example.com');
    expect(links[0]?.getAttribute('rel')).toContain('noopener');
    expect(container.textContent).toContain('bad');
  });
});
