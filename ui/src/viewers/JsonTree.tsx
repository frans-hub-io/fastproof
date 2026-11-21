import { UnstyledButton } from '@mantine/core';
import { useState, type ReactNode } from 'react';

function Scalar({ value }: { value: unknown }) {
  if (typeof value === 'string') return <span className="fp-json-string">{JSON.stringify(value)}</span>;
  if (typeof value === 'number') return <span className="fp-json-number">{String(value)}</span>;
  return <span className="fp-json-literal">{typeof value === 'boolean' ? String(value) : value === null ? 'null' : 'undefined'}</span>;
}

const MAX_CHILDREN = 500;

function Node({ name, value, depth, last }: { name?: string; value: unknown; depth: number; last: boolean }) {
  const [open, setOpen] = useState(depth < 3);
  const comma = last ? '' : ',';
  const label: ReactNode = name !== undefined ? <span className="fp-json-key">{JSON.stringify(name)}: </span> : null;
  if (value === null || typeof value !== 'object') {
    return (
      <div style={{ paddingLeft: depth ? 16 : 0 }}>
        {label}
        <Scalar value={value} />
        {comma}
      </div>
    );
  }
  const entries: [string | undefined, unknown][] = Array.isArray(value) ? value.map((v) => [undefined, v]) : Object.entries(value as Record<string, unknown>);
  const [openCh, closeCh] = Array.isArray(value) ? ['[', ']'] : ['{', '}'];
  if (!entries.length) {
    return (
      <div style={{ paddingLeft: depth ? 16 : 0 }}>
        {label}
        {openCh}
        {closeCh}
        {comma}
      </div>
    );
  }
  return (
    <div style={{ paddingLeft: depth ? 16 : 0 }}>
      <UnstyledButton onClick={() => setOpen((o) => !o)} aria-expanded={open} style={{ font: 'inherit', color: 'inherit' }}>
        <span aria-hidden style={{ display: 'inline-block', width: 12 }}>
          {open ? '▾' : '▸'}
        </span>
        {label}
        {openCh}
        {!open && (
          <span style={{ opacity: 0.6 }}>
            {' '}
            {String(entries.length)} {Array.isArray(value) ? 'items' : 'keys'} {closeCh}
            {comma}
          </span>
        )}
      </UnstyledButton>
      {open && (
        <>
          {entries.slice(0, MAX_CHILDREN).map(([k, v], i) => (
            <Node key={k ?? i} name={k} value={v} depth={depth + 1} last={i === Math.min(entries.length, MAX_CHILDREN) - 1} />
          ))}
          {entries.length > MAX_CHILDREN && <div style={{ paddingLeft: 16, opacity: 0.6 }}>… {String(entries.length - MAX_CHILDREN)} more</div>}
          <div>
            {closeCh}
            {comma}
          </div>
        </>
      )}
    </div>
  );
}

export function JsonTree({ value }: { value: unknown }) {
  return (
    <div className="fp-log" data-testid="json-tree">
      <Node value={value} depth={0} last />
    </div>
  );
}
