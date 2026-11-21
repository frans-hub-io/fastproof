import { Badge, Group, Modal, Stack, Text, UnstyledButton } from '@mantine/core';
import { useMediaQuery } from '@mantine/hooks';
import { IconBraces, IconExternalLink, IconFile, IconFileText, IconLayersSubtract, IconMovie, IconPhoto, IconRoute, IconWorld } from '@tabler/icons-react';
import { useState } from 'react';
import { attachmentUrl, traceViewerUrl } from '../api/client';
import type { Attachment } from '../api/types';
import { attachmentKind, groupAttachments, isActiveContent, type AttachmentEntry } from '../lib/attachments';
import { formatBytes } from '../lib/format';
import { DownloadLinks, EntryBody } from './AttachmentViewer';

const KIND_LABEL: Record<string, string> = {
  image: 'image',
  video: 'video',
  trace: 'trace',
  imageDiff: 'image diff',
  httpExchange: 'HTTP',
  text: 'text',
  log: 'log',
  json: 'JSON',
  other: 'file',
  playwrightDiff: 'image diff',
};

function iconFor(kind: string) {
  switch (kind) {
    case 'image':
      return <IconPhoto size={16} />;
    case 'video':
      return <IconMovie size={16} />;
    case 'trace':
      return <IconRoute size={16} />;
    case 'imageDiff':
    case 'playwrightDiff':
      return <IconLayersSubtract size={16} />;
    case 'httpExchange':
      return <IconWorld size={16} />;
    case 'json':
      return <IconBraces size={16} />;
    case 'text':
    case 'log':
      return <IconFileText size={16} />;
    default:
      return <IconFile size={16} />;
  }
}

function entryInfo(e: AttachmentEntry): { key: string; name: string; kind: string; size: number; active: boolean } {
  if (e.kind === 'single') {
    const a = e.attachment;
    return { key: a.id, name: a.name, kind: attachmentKind(a), size: a.size, active: isActiveContent(a) };
  }
  const parts = [e.expected, e.actual, e.diff].filter((x): x is Attachment => !!x);
  return { key: `pw:${e.name}`, name: `${e.name} (expected / actual${e.diff ? ' / diff' : ''})`, kind: 'playwrightDiff', size: parts.reduce((s, a) => s + a.size, 0), active: false };
}

export function AttachmentList({ project, attachments, compact = false }: { project: string; attachments: readonly Attachment[]; compact?: boolean }) {
  const [open, setOpen] = useState<AttachmentEntry | null>(null);
  const phone = useMediaQuery('(max-width: 48em)');
  if (!attachments.length) return null;
  const entries = groupAttachments(attachments);
  const current = open ? entryInfo(open) : null;
  return (
    <>
      <Stack gap={4} data-testid="attachment-list">
        {entries.map((e) => {
          const info = entryInfo(e);
          const single = e.kind === 'single' ? e.attachment : null;
          const external = info.active && single ? attachmentUrl(project, single.id) : single && info.kind === 'trace' ? traceViewerUrl(project, single.id) : null;
          const content = (
            <Group gap="xs" wrap="nowrap" style={{ minWidth: 0 }}>
              <span style={{ color: 'var(--mantine-color-dimmed)', display: 'inline-flex' }}>{iconFor(info.kind)}</span>
              <Text size="sm" className="fh-truncate" style={{ minWidth: 0, flex: 1 }}>
                {info.name}
              </Text>
              {!compact && (
                <Badge size="xs" variant="light" color="gray" radius="sm" style={{ textTransform: 'none', flex: 'none' }}>
                  {KIND_LABEL[info.kind] ?? info.kind}
                </Badge>
              )}
              <Text size="xs" c="dimmed" style={{ flex: 'none' }}>
                {formatBytes(info.size)}
              </Text>
              {external && <IconExternalLink size={14} style={{ flex: 'none' }} />}
            </Group>
          );
          return external ? (
            <UnstyledButton
              key={info.key}
              component="a"
              href={external}
              target="_blank"
              rel="noopener noreferrer"
              className="fp-tree-row"
              data-testid="attachment-item"
              data-kind={info.kind}
              aria-label={`${info.name} (opens in a new tab)`}
            >
              {content}
            </UnstyledButton>
          ) : (
            <UnstyledButton key={info.key} onClick={() => setOpen(e)} className="fp-tree-row" data-testid="attachment-item" data-kind={info.kind}>
              {content}
            </UnstyledButton>
          );
        })}
      </Stack>
      <Modal opened={!!open} onClose={() => setOpen(null)} title={current?.name ?? ''} size="min(1100px, 96vw)" fullScreen={phone} styles={{ title: { fontWeight: 700, wordBreak: 'break-word' } }}>
        {open && (
          <Stack gap="md" data-testid="attachment-viewer">
            <EntryBody project={project} entry={open} />
            <DownloadLinks project={project} entry={open} />
          </Stack>
        )}
      </Modal>
    </>
  );
}
