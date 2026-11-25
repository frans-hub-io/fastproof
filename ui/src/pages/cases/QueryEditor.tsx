import { autocompletion, startCompletion, type Completion, type CompletionContext, type CompletionResult } from '@codemirror/autocomplete';
import { history, historyKeymap, standardKeymap } from '@codemirror/commands';
import { setDiagnostics } from '@codemirror/lint';
import { Annotation, EditorState, type Extension } from '@codemirror/state';
import { Decoration, EditorView, ViewPlugin, keymap, placeholder, type DecorationSet, type ViewUpdate } from '@codemirror/view';
import { useEffect, useRef } from 'react';
import type { QueryError } from '../../api/queries';
import { cspNonce } from '../../lib/cspNonce';
import { analyze, completionsAt, type CompletionSchema, type Suggestion } from '../../lib/tm/fpql';

const external = Annotation.define<boolean>();

const KIND: Record<Suggestion['kind'], string> = { attribute: 'property', operator: 'keyword', value: 'text', function: 'function', keyword: 'keyword', field: 'variable' };

function highlighter(): Extension {
  const build = (view: EditorView): DecorationSet =>
    Decoration.set(
      analyze(view.state.doc.toString())
        .tokens.filter((t) => t.to > t.from)
        .map((t) => Decoration.mark({ class: `cm-fp-${t.role}` }).range(t.from, t.to)),
    );
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) {
        this.decorations = build(view);
      }
      update(u: ViewUpdate) {
        if (u.docChanged) this.decorations = build(u.view);
      }
    },
    { decorations: (v) => v.decorations },
  );
}

const theme = EditorView.theme({
  '&': {
    backgroundColor: 'var(--mantine-color-body)',
    color: 'var(--mantine-color-text)',
    border: '1px solid var(--mantine-color-default-border)',
    borderRadius: 'var(--mantine-radius-sm)',
    fontSize: 'var(--mantine-font-size-sm)',
    minWidth: '0',
  },
  '&.cm-focused': { outline: '2px solid var(--mantine-primary-color-filled)', outlineOffset: '-1px' },
  '.cm-scroller': { fontFamily: 'var(--mantine-font-family-monospace)', lineHeight: '1.5', overflowX: 'auto' },
  '.cm-content': { padding: '7px 8px', caretColor: 'var(--mantine-color-text)', minHeight: '32px' },
  '.cm-line': { padding: '0' },
  '.cm-cursor, .cm-dropCursor': { borderLeftColor: 'var(--mantine-color-text)' },
  '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground': { background: 'var(--mantine-primary-color-light-hover)' },
  '.cm-placeholder': { color: 'var(--mantine-color-placeholder)' },
  '.cm-fp-keyword': { color: 'light-dark(var(--mantine-color-grape-8), var(--mantine-color-grape-4))', fontWeight: '600' },
  '.cm-fp-attribute': { color: 'light-dark(var(--mantine-color-blue-8), var(--mantine-color-blue-4))' },
  '.cm-fp-fieldkey': { color: 'light-dark(var(--mantine-color-cyan-9), var(--mantine-color-cyan-4))' },
  '.cm-fp-string': { color: 'light-dark(var(--mantine-color-teal-9), var(--mantine-color-teal-4))' },
  '.cm-fp-value': { color: 'light-dark(var(--mantine-color-teal-9), var(--mantine-color-teal-4))' },
  '.cm-fp-number': { color: 'light-dark(var(--mantine-color-orange-9), var(--mantine-color-orange-4))' },
  '.cm-fp-function': { color: 'light-dark(var(--mantine-color-indigo-8), var(--mantine-color-indigo-3))' },
  '.cm-fp-operator': { color: 'var(--mantine-color-dimmed)' },
  '.cm-fp-punct': { color: 'var(--mantine-color-dimmed)' },
  '.cm-fp-error': { color: 'light-dark(var(--mantine-color-red-8), var(--mantine-color-red-4))' },
  '.cm-lintRange-error': { backgroundImage: 'none', textDecoration: 'underline wavy var(--mantine-color-red-filled)', textUnderlineOffset: '3px' },
  '.cm-tooltip': {
    backgroundColor: 'var(--mantine-color-body)',
    color: 'var(--mantine-color-text)',
    border: '1px solid var(--mantine-color-default-border)',
    borderRadius: 'var(--mantine-radius-sm)',
    boxShadow: 'var(--mantine-shadow-md)',
    maxWidth: 'min(480px, 92vw)',
  },
  '.cm-tooltip-autocomplete > ul': { fontFamily: 'var(--mantine-font-family-monospace)', fontSize: 'var(--mantine-font-size-xs)', maxHeight: '260px' },
  '.cm-tooltip-autocomplete > ul > li': { padding: '3px 8px', display: 'flex', gap: '8px', alignItems: 'baseline' },
  '.cm-tooltip-autocomplete > ul > li[aria-selected]': { backgroundColor: 'var(--mantine-primary-color-light)', color: 'var(--mantine-color-text)' },
  '.cm-completionDetail': {
    color: 'var(--mantine-color-dimmed)',
    fontStyle: 'normal',
    marginLeft: 'auto',
    fontFamily: 'var(--mantine-font-family)',
    overflow: 'hidden',
    textOverflow: 'ellipsis',
    whiteSpace: 'nowrap',
  },
  '.cm-completionIcon': { display: 'none' },
  '.cm-diagnostic': { padding: '4px 8px', borderLeft: '3px solid var(--mantine-color-red-filled)', fontFamily: 'var(--mantine-font-family)' },
});

const BOOST: Record<Suggestion['kind'], number> = { value: 3, field: 2, attribute: 1, operator: 1, keyword: -1, function: -2 };

function toCompletion(s: Suggestion): Completion {
  const trigger = s.insert.endsWith(' ') || s.insert.endsWith('[');
  return {
    label: s.label,
    detail: s.detail,
    type: KIND[s.kind],
    boost: BOOST[s.kind],
    apply: (view, _c, from, to) => {
      view.dispatch({ changes: { from, to, insert: s.insert }, selection: { anchor: from + s.insert.length - (s.cursorBack ?? 0) }, userEvent: 'input.complete' });
      if (trigger || s.cursorBack) setTimeout(() => startCompletion(view), 0);
    },
  };
}

export interface QueryEditorProps {
  value: string;
  onChange: (value: string) => void;
  onApply: (value: string) => void;
  schema: CompletionSchema | undefined;
  error: QueryError | null;
  placeholder?: string;
  autoFocus?: boolean;
}

export default function QueryEditor(props: QueryEditorProps) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const latest = useRef(props);
  useEffect(() => {
    latest.current = props;
  });

  useEffect(() => {
    if (!host.current) return;
    const source = (ctx: CompletionContext): CompletionResult | null => {
      const schema = latest.current.schema;
      if (!schema) return null;
      const before = ctx.state.sliceDoc(0, ctx.pos);
      if (!ctx.explicit && before.trim() === '' && ctx.pos === 0) return null;
      const r = completionsAt(before, schema);
      if (!r) return null;
      const typed = before.slice(r.from);
      const options = r.options.filter((o) => typed === '' || (o.insert !== typed && o.label !== typed));
      return options.length ? { from: r.from, options: options.map(toCompletion), filter: true } : null;
    };
    const nonce = cspNonce();
    const extensions: Extension[] = [
      history(),
      keymap.of([
        {
          key: 'Enter',
          run: (v) => {
            latest.current.onApply(v.state.doc.toString());
            return true;
          },
        },
        { key: 'Shift-Enter', run: () => true },
        {
          key: 'Escape',
          run: (v) => {
            v.contentDOM.blur();
            return true;
          },
        },
        ...historyKeymap,
        ...standardKeymap,
      ]),
      autocompletion({ override: [source], icons: false, defaultKeymap: true }),
      highlighter(),
      placeholder(latest.current.placeholder ?? ''),
      EditorView.lineWrapping,
      EditorState.transactionFilter.of((tr) => (tr.docChanged && tr.newDoc.lines > 1 ? [] : tr)),
      EditorView.domEventHandlers({
        paste(e, v) {
          const text = e.clipboardData?.getData('text/plain');
          if (!text || !/[\r\n]/.test(text)) return false;
          e.preventDefault();
          v.dispatch(v.state.replaceSelection(text.replace(/\s*[\r\n]+\s*/g, ' ')), { userEvent: 'input.paste' });
          return true;
        },
      }),
      EditorView.contentAttributes.of({
        'aria-label': 'Query',
        'aria-multiline': 'false',
        'data-testid': 'query-bar',
        'data-mantine-stop-propagation': 'true',
        spellcheck: 'false',
        autocapitalize: 'off',
        autocorrect: 'off',
      }),
      EditorView.updateListener.of((u) => {
        if (u.docChanged && !u.transactions.some((t) => t.annotation(external))) latest.current.onChange(u.state.doc.toString());
      }),
      theme,
    ];
    if (nonce) extensions.push(EditorView.cspNonce.of(nonce));
    const v = new EditorView({ parent: host.current, state: EditorState.create({ doc: latest.current.value, extensions }) });
    view.current = v;
    if (latest.current.autoFocus) v.focus();
    return () => {
      v.destroy();
      view.current = null;
    };
  }, []);

  const { value, error } = props;
  useEffect(() => {
    const v = view.current;
    if (v && v.state.doc.toString() !== value) v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: value }, annotations: external.of(true) });
  }, [value]);

  useEffect(() => {
    const v = view.current;
    if (!v) return;
    const n = v.state.doc.length;
    if (!error || n === 0) {
      v.dispatch(setDiagnostics(v.state, []));
      return;
    }
    let from = Math.min(Math.max(error.position, 0), n);
    const to = Math.min(from + Math.max(error.length, 1), n);
    if (from === to) from = Math.max(0, to - 1);
    v.dispatch(setDiagnostics(v.state, [{ from, to, severity: 'error', message: error.message }]));
  }, [error, value]);

  return <div ref={host} data-testid="query-editor" style={{ minWidth: 0, width: '100%' }} />;
}
