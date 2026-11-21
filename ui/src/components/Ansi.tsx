import { useMemo, type CSSProperties } from 'react';
import { parseAnsi, type AnsiStyle } from '../lib/ansi';

function css(s: AnsiStyle): CSSProperties | undefined {
  if (!s.fg && !s.bg && !s.bold && !s.dim && !s.italic && !s.underline) return undefined;
  return {
    color: s.fg,
    background: s.bg,
    fontWeight: s.bold ? 700 : undefined,
    opacity: s.dim ? 0.7 : undefined,
    fontStyle: s.italic ? 'italic' : undefined,
    textDecoration: s.underline ? 'underline' : undefined,
  };
}

export function AnsiText({ text, maxHeight, testId }: { text: string; maxHeight?: number; testId?: string }) {
  const segments = useMemo(() => parseAnsi(text), [text]);
  return (
    <pre className="fp-log" style={maxHeight ? { maxHeight } : undefined} data-testid={testId}>
      {segments.map((s, i) => {
        const style = css(s.style);
        return style ? (
          <span key={i} style={style}>
            {s.text}
          </span>
        ) : (
          s.text
        );
      })}
    </pre>
  );
}
