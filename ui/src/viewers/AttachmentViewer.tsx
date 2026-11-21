import { Alert, Anchor, Button, Group, SegmentedControl, Stack, Text } from '@mantine/core';
import { IconDownload, IconExternalLink, IconPlayerPlay } from '@tabler/icons-react';
import { useState } from 'react';
import { attachmentUrl, errorMessage, traceViewerUrl } from '../api/client';
import type { AllureImageDiff, Attachment } from '../api/types';
import { AnsiText } from '../components/Ansi';
import { Loading } from '../components/QueryState';
import { attachmentKind, isActiveContent, type AttachmentEntry } from '../lib/attachments';
import { formatBytes } from '../lib/format';
import { HttpExchangeView } from './HttpExchangeView';
import { safeDataImage } from '../lib/images';
import { ImageCompare } from './ImageCompare';
import { JsonTree } from './JsonTree';
import { useAttachmentJson, useAttachmentText } from './useAttachmentText';

function Problem({ message }: { message: string }) {
  return (
    <Alert color="red" variant="light" title="Cannot show this attachment">
      {message}
    </Alert>
  );
}

function TextView({ project, a }: { project: string; a: Attachment }) {
  const q = useAttachmentText(project, a.id);
  if (q.isPending) return <Loading />;
  if (q.isError) return <Problem message={errorMessage(q.error)} />;
  return (
    <Stack gap="xs">
      <AnsiText text={q.data.text} maxHeight={640} testId="text-viewer" />
      {q.data.truncated && (
        <Text size="xs" c="dimmed">
          Showing the first 2 MB of {formatBytes(a.size)}. Download the attachment for the rest.
        </Text>
      )}
    </Stack>
  );
}

function JsonView({ project, a }: { project: string; a: Attachment }) {
  const { query, parsed } = useAttachmentJson(project, a.id);
  const [raw, setRaw] = useState(false);
  if (query.isPending) return <Loading />;
  if (query.isError) return <Problem message={errorMessage(query.error)} />;
  if (!parsed?.ok) return <TextView project={project} a={a} />;
  return (
    <Stack gap="xs">
      <SegmentedControl
        size="xs"
        value={raw ? 'raw' : 'tree'}
        onChange={(v) => setRaw(v === 'raw')}
        data={[
          { value: 'tree', label: 'Tree' },
          { value: 'raw', label: 'Pretty' },
        ]}
        style={{ alignSelf: 'flex-start' }}
      />
      {raw ? <pre className="fp-log">{JSON.stringify(parsed.value, null, 2)}</pre> : <JsonTree value={parsed.value} />}
    </Stack>
  );
}

function HttpView({ project, a }: { project: string; a: Attachment }) {
  const { query, parsed } = useAttachmentJson(project, a.id);
  if (query.isPending) return <Loading />;
  if (query.isError) return <Problem message={errorMessage(query.error)} />;
  if (!parsed) return null;
  if (!parsed.ok) return <Problem message={parsed.error} />;
  if (!parsed.value || typeof parsed.value !== 'object') return <Problem message="Not an HTTP exchange object." />;
  return <HttpExchangeView exchange={parsed.value} />;
}

function AllureDiffView({ project, a }: { project: string; a: Attachment }) {
  const { query, parsed } = useAttachmentJson(project, a.id);
  if (query.isPending) return <Loading />;
  if (query.isError) return <Problem message={errorMessage(query.error)} />;
  if (!parsed) return null;
  if (!parsed.ok) return <Problem message={parsed.error} />;
  const v = (parsed.value && typeof parsed.value === 'object' ? parsed.value : {}) as AllureImageDiff;
  const expected = safeDataImage(v.expected);
  const actual = safeDataImage(v.actual);
  const diff = safeDataImage(v.diff);
  if (!expected || !actual) {
    if (diff) return <img src={diff} alt={`${a.name}: diff`} style={{ maxWidth: '100%' }} />;
    return <Problem message="The image diff has no expected and actual images (PNG, JPEG, GIF or WebP data URIs)." />;
  }
  return <ImageCompare images={{ expected, actual, diff }} name={a.name} />;
}

function TraceView({ project, a }: { project: string; a: Attachment }) {
  return (
    <Stack gap="sm" align="flex-start">
      <Text size="sm">The Playwright trace opens in the trace viewer hosted by this server.</Text>
      <Button component="a" href={traceViewerUrl(project, a.id)} target="_blank" rel="noopener noreferrer" leftSection={<IconPlayerPlay size={16} />} data-testid="trace-link">
        Open in trace viewer
      </Button>
    </Stack>
  );
}

function ActiveView({ project, a }: { project: string; a: Attachment }) {
  return (
    <Stack gap="sm" align="flex-start">
      <Text size="sm">
        {a.contentType} attachments are not displayed inside FastProof. They open from the attachment address in a separate, sandboxed tab.
      </Text>
      <Button component="a" href={attachmentUrl(project, a.id)} target="_blank" rel="noopener noreferrer" leftSection={<IconExternalLink size={16} />} data-testid="open-new-tab">
        Open in a new tab
      </Button>
    </Stack>
  );
}

export function AttachmentBody({ project, attachment: a }: { project: string; attachment: Attachment }) {
  if (isActiveContent(a)) return <ActiveView project={project} a={a} />;
  const url = attachmentUrl(project, a.id);
  switch (attachmentKind(a)) {
    case 'image':
      return (
        <Anchor href={url} target="_blank" rel="noopener noreferrer">
          <img src={url} alt={a.name} style={{ maxWidth: '100%', height: 'auto', display: 'block', borderRadius: 8 }} data-testid="image-viewer" />
        </Anchor>
      );
    case 'video':
      return (
        <video src={url} controls preload="metadata" style={{ width: '100%', maxHeight: '70vh', borderRadius: 8, background: '#000' }} data-testid="video-viewer">
          <track kind="captions" />
        </video>
      );
    case 'imageDiff':
      return <AllureDiffView project={project} a={a} />;
    case 'httpExchange':
      return <HttpView project={project} a={a} />;
    case 'json':
      return <JsonView project={project} a={a} />;
    case 'text':
    case 'log':
      return <TextView project={project} a={a} />;
    case 'trace':
      return <TraceView project={project} a={a} />;
    default:
      if (/^text\//i.test(a.contentType)) return <TextView project={project} a={a} />;
      return (
        <Text size="sm" c="dimmed">
          No preview for {a.contentType || 'this type'}. Download the file to open it.
        </Text>
      );
  }
}

export function EntryBody({ project, entry }: { project: string; entry: AttachmentEntry }) {
  if (entry.kind === 'single') return <AttachmentBody project={project} attachment={entry.attachment} />;
  if (!entry.expected || !entry.actual) return null;
  return (
    <ImageCompare
      name={entry.name}
      images={{
        expected: attachmentUrl(project, entry.expected.id),
        actual: attachmentUrl(project, entry.actual.id),
        diff: entry.diff ? attachmentUrl(project, entry.diff.id) : undefined,
      }}
    />
  );
}

export function DownloadLinks({ project, entry }: { project: string; entry: AttachmentEntry }) {
  const list = entry.kind === 'single' ? [entry.attachment] : [entry.expected, entry.actual, entry.diff].filter((x): x is Attachment => !!x);
  return (
    <Group gap="xs" wrap="wrap">
      {list.map((a) => (
        <Button key={a.id} component="a" href={attachmentUrl(project, a.id, true)} variant="default" size="xs" leftSection={<IconDownload size={14} />} download={a.name}>
          {list.length > 1 ? a.name : `Download (${formatBytes(a.size)})`}
        </Button>
      ))}
    </Group>
  );
}
