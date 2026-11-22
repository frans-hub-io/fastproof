import { Group, Loader, SegmentedControl, Stack, Text, Textarea } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { useEffect, useRef, useState, type ClipboardEvent, type KeyboardEvent, type ReactNode } from 'react';
import { errorMessage } from '../../api/client';
import { markdownPreview, uploadCaseAttachment } from '../../api/tm';
import { CaseHtml } from './CaseHtml';

export const MAX_MARKDOWN = 16384;

/** Debounced server preview (the same renderer as the stored HTML). */
function usePreview(project: string, text: string, on: boolean): { html: string; loading: boolean; error: string | null } {
  const [state, setState] = useState<{ html: string; loading: boolean; error: string | null }>({ html: '', loading: false, error: null });
  useEffect(() => {
    if (!on) return;
    const ctrl = new AbortController();
    setState((s) => ({ ...s, loading: true }));
    const t = setTimeout(() => {
      markdownPreview(project, text, ctrl.signal)
        .then((html) => setState({ html, loading: false, error: null }))
        .catch((e: unknown) => {
          if (!ctrl.signal.aborted) setState({ html: '', loading: false, error: errorMessage(e) });
        });
    }, 300);
    return () => {
      clearTimeout(t);
      ctrl.abort();
    };
  }, [project, text, on]);
  return state;
}

export interface MarkdownFieldProps {
  project: string;
  caseNumber: number | null;
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  minRows?: number;
  locked?: ReactNode;
  compact?: boolean;
  onKeyDown?: (e: KeyboardEvent<HTMLTextAreaElement>) => void;
  onFocus?: () => void;
  testId?: string;
  autoFocus?: boolean;
  inputId?: string;
}

/**
 * Markdown in a plain textarea with a Write / Preview switch. Pasting an image uploads it as a case attachment
 * and inserts `![name](attachment:<id>)` at the caret.
 */
export function MarkdownField({ project, caseNumber, label, value, onChange, placeholder, minRows = 2, locked, compact, onKeyDown, onFocus, testId, autoFocus, inputId }: MarkdownFieldProps) {
  const [mode, setMode] = useState<'write' | 'preview'>('write');
  const [uploading, setUploading] = useState(0);
  const ref = useRef<HTMLTextAreaElement>(null);
  const preview = usePreview(project, value, mode === 'preview');
  const over = value.length > MAX_MARKDOWN;

  const insert = (text: string) => {
    const el = ref.current;
    const start = el?.selectionStart ?? value.length;
    const end = el?.selectionEnd ?? value.length;
    const next = `${value.slice(0, start)}${text}${value.slice(end)}`;
    onChange(next);
    requestAnimationFrame(() => {
      el?.setSelectionRange(start + text.length, start + text.length);
    });
  };

  const onPaste = (e: ClipboardEvent<HTMLTextAreaElement>) => {
    const files = Array.from(e.clipboardData.files).filter((f) => f.type.startsWith('image/'));
    if (!files.length || caseNumber === null) return;
    e.preventDefault();
    for (const f of files.slice(0, 5)) {
      const name = f.name && f.name !== 'image.png' ? f.name : `pasted-${new Date().toISOString().replace(/[:.]/g, '-')}.png`;
      setUploading((n) => n + 1);
      uploadCaseAttachment(project, caseNumber, f, name)
        .then((a) => insert(`![${name.replace(/[[\]]/g, '')}](attachment:${a.id})`))
        .catch((err: unknown) => notifications.show({ color: 'red', title: 'Image not uploaded', message: errorMessage(err) }))
        .finally(() => setUploading((n) => n - 1));
    }
  };

  if (locked) {
    return (
      <Stack gap={4}>
        <Group gap={6}>
          <Text size="sm" fw={500}>
            {label}
          </Text>
          {locked}
        </Group>
        <Text size="sm" c="dimmed" style={{ whiteSpace: 'pre-wrap' }} data-testid={testId ? `${testId}-locked` : undefined}>
          {value || '—'}
        </Text>
      </Stack>
    );
  }

  return (
    <Stack gap={4} className="fp-tm-md-field">
      <Group justify="space-between" gap={6} wrap="nowrap">
        <Text size={compact ? 'xs' : 'sm'} fw={500} component="label" htmlFor={inputId} c={compact ? 'dimmed' : undefined}>
          {label}
        </Text>
        <Group gap={6} wrap="nowrap">
          {uploading > 0 && <Loader size={12} aria-label="Uploading image" />}
          <SegmentedControl
            size="xs"
            value={mode}
            onChange={(v) => setMode(v === 'preview' ? 'preview' : 'write')}
            data={[
              { value: 'write', label: 'Write' },
              { value: 'preview', label: 'Preview' },
            ]}
            aria-label={`${label}: write or preview`}
          />
        </Group>
      </Group>
      {mode === 'write' ? (
        <Textarea
          ref={ref}
          id={inputId}
          value={value}
          onChange={(e) => onChange(e.currentTarget.value)}
          onPaste={onPaste}
          onKeyDown={onKeyDown}
          onFocus={onFocus}
          autosize
          minRows={minRows}
          maxRows={compact ? 10 : 24}
          placeholder={placeholder}
          error={over ? `At most ${String(MAX_MARKDOWN)} characters` : undefined}
          data-testid={testId}
          autoFocus={autoFocus}
          aria-label={inputId ? undefined : label}
          className="fp-mono-input"
        />
      ) : (
        <div className="fp-tm-preview-box" data-testid={testId ? `${testId}-preview` : undefined} aria-live="polite">
          {preview.loading && !preview.html ? <Loader size="xs" /> : preview.error ? <Text c="red" size="sm">{preview.error}</Text> : <CaseHtml html={preview.html} project={project} empty={<Text size="sm" c="dimmed">Nothing to preview.</Text>} />}
        </div>
      )}
    </Stack>
  );
}
