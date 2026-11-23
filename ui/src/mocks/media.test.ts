import { describe, expect, it } from 'vitest';
import { parseRange } from './media';

describe('mock media ranges', () => {
  it('parses the forms a browser sends', () => {
    expect(parseRange(null, 100)).toBe('full');
    expect(parseRange('bytes=0-', 100)).toEqual({ start: 0, end: 99 });
    expect(parseRange('bytes=10-19', 100)).toEqual({ start: 10, end: 19 });
    expect(parseRange('bytes=90-500', 100)).toEqual({ start: 90, end: 99 });
    expect(parseRange('bytes=-10', 100)).toEqual({ start: 90, end: 99 });
    expect(parseRange('bytes=100-', 100)).toBeNull();
    expect(parseRange('garbage', 100)).toBe('full');
  });
});
