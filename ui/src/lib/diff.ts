export type DiffOp = 'equal' | 'add' | 'remove';

export interface DiffLine {
  op: DiffOp;
  text: string;
}

const MAX_CELLS = 4_000_000;

export function diffLines(expected: string, actual: string): DiffLine[] {
  const a = expected.split('\n');
  const b = actual.split('\n');
  let start = 0;
  while (start < a.length && start < b.length && a[start] === b[start]) start++;
  let endA = a.length;
  let endB = b.length;
  while (endA > start && endB > start && a[endA - 1] === b[endB - 1]) {
    endA--;
    endB--;
  }
  const head: DiffLine[] = a.slice(0, start).map((text) => ({ op: 'equal', text }));
  const tail: DiffLine[] = a.slice(endA).map((text) => ({ op: 'equal', text }));
  const midA = a.slice(start, endA);
  const midB = b.slice(start, endB);
  const n = midA.length;
  const m = midB.length;
  if (n * m > MAX_CELLS) {
    return [...head, ...midA.map((text) => ({ op: 'remove' as const, text })), ...midB.map((text) => ({ op: 'add' as const, text })), ...tail];
  }
  const width = m + 1;
  const lcs = new Uint32Array((n + 1) * width);
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      lcs[i * width + j] = midA[i] === midB[j] ? (lcs[(i + 1) * width + j + 1] ?? 0) + 1 : Math.max(lcs[(i + 1) * width + j] ?? 0, lcs[i * width + j + 1] ?? 0);
    }
  }
  const mid: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (midA[i] === midB[j]) {
      mid.push({ op: 'equal', text: midA[i] ?? '' });
      i++;
      j++;
    } else if ((lcs[(i + 1) * width + j] ?? 0) >= (lcs[i * width + j + 1] ?? 0)) {
      mid.push({ op: 'remove', text: midA[i] ?? '' });
      i++;
    } else {
      mid.push({ op: 'add', text: midB[j] ?? '' });
      j++;
    }
  }
  while (i < n) mid.push({ op: 'remove', text: midA[i++] ?? '' });
  while (j < m) mid.push({ op: 'add', text: midB[j++] ?? '' });
  return [...head, ...mid, ...tail];
}
