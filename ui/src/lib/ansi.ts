export interface AnsiStyle {
  fg?: string;
  bg?: string;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
}

export interface AnsiSegment {
  text: string;
  style: AnsiStyle;
}

const CUBE = [0, 95, 135, 175, 215, 255];

function palette256(n: number): string | undefined {
  if (n < 0 || n > 255) return undefined;
  if (n < 16) return `var(--fp-ansi-${String(n)})`;
  if (n >= 232) {
    const v = 8 + (n - 232) * 10;
    return `rgb(${String(v)}, ${String(v)}, ${String(v)})`;
  }
  const i = n - 16;
  const r = CUBE[Math.floor(i / 36)] ?? 0;
  const g = CUBE[Math.floor(i / 6) % 6] ?? 0;
  const b = CUBE[i % 6] ?? 0;
  return `rgb(${String(r)}, ${String(g)}, ${String(b)})`;
}

function clampByte(v: number | undefined): number {
  return Math.max(0, Math.min(255, Math.trunc(v ?? 0)));
}

function applySgr(style: AnsiStyle, codes: number[]): AnsiStyle {
  let s: AnsiStyle = { ...style };
  for (let i = 0; i < codes.length; i++) {
    const c = codes[i] ?? 0;
    if (c === 0) s = {};
    else if (c === 1) s.bold = true;
    else if (c === 2) s.dim = true;
    else if (c === 3) s.italic = true;
    else if (c === 4) s.underline = true;
    else if (c === 22) {
      s.bold = false;
      s.dim = false;
    } else if (c === 23) s.italic = false;
    else if (c === 24) s.underline = false;
    else if (c >= 30 && c <= 37) s.fg = `var(--fp-ansi-${String(c - 30)})`;
    else if (c >= 90 && c <= 97) s.fg = `var(--fp-ansi-${String(c - 90 + 8)})`;
    else if (c >= 40 && c <= 47) s.bg = `var(--fp-ansi-${String(c - 40)})`;
    else if (c >= 100 && c <= 107) s.bg = `var(--fp-ansi-${String(c - 100 + 8)})`;
    else if (c === 39) delete s.fg;
    else if (c === 49) delete s.bg;
    else if (c === 38 || c === 48) {
      const mode = codes[i + 1];
      let color: string | undefined;
      if (mode === 5) {
        color = palette256(codes[i + 2] ?? -1);
        i += 2;
      } else if (mode === 2) {
        color = `rgb(${String(clampByte(codes[i + 2]))}, ${String(clampByte(codes[i + 3]))}, ${String(clampByte(codes[i + 4]))})`;
        i += 4;
      }
      if (color) {
        if (c === 38) s.fg = color;
        else s.bg = color;
      }
    }
  }
  return s;
}

const ESC = '\u001b';
const SEQUENCE = new RegExp(`${ESC}\\[([0-9;?]*)([A-Za-z])|${ESC}\\][^\u0007${ESC}]*(?:\u0007|${ESC}\\\\)|${ESC}[@-Z\\\\-_]`, 'g');

export function parseAnsi(input: string): AnsiSegment[] {
  const out: AnsiSegment[] = [];
  let style: AnsiStyle = {};
  let last = 0;
  const push = (text: string) => {
    if (!text) return;
    const prev = out[out.length - 1];
    if (prev && prev.style === style) prev.text += text;
    else out.push({ text, style });
  };
  for (const m of input.matchAll(SEQUENCE)) {
    push(input.slice(last, m.index));
    last = m.index + m[0].length;
    if (m[2] === 'm') {
      const params = (m[1] ?? '').replace(/\?/g, '');
      const codes = params === '' ? [0] : params.split(';').map((p) => (p === '' ? 0 : Number(p)));
      style = applySgr(style, codes);
    }
  }
  push(input.slice(last));
  return out;
}

export function stripAnsi(input: string): string {
  return input.replace(SEQUENCE, '');
}

export function hasAnsi(input: string): boolean {
  return input.includes(ESC);
}
