import { Alert, Anchor, Button, Group, SegmentedControl, Stack, Text } from '@mantine/core';
import { IconDownload, IconExternalLink, IconPlayerPlay } from '@tabler/icons-react';
import { useState } from 'react';
import { notifications } from '@mantine/notifications';
import { usesToken } from '../lib/authToken';
import { downloadWithToken, useAuthedUrl } from './authedUrl';
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

function TokenOnly({ what }: { what: string }) {
  return (
    <Text size="sm" c="dimmed" data-testid="token-mode-note">
      {what} needs a browser session, which an API token does not create. Download the file below, or sign in with SSO.
    </Text>
  );
}

function TraceView({ project, a }: { project: string; a: Attachment }) {
  if (usesToken()) return <TokenOnly what="The trace viewer" />;
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
  if (usesToken()) return <TokenOnly what={`Opening ${a.contentType} attachments in a sandboxed tab`} />;
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

function ImageView({ url, name }: { url: string; name: string }) {
  const { src, error } = useAuthedUrl(url);
  if (error) return <Problem message={error} />;
  if (!src) return <Loading />;
  const img = <img src={src} alt={name} style={{ maxWidth: '100%', height: 'auto', display: 'block', borderRadius: 8 }} data-testid="image-viewer" />;
  return usesToken() ? (
    img
  ) : (
    <Anchor href={url} target="_blank" rel="noopener noreferrer">
      {img}
    </Anchor>
  );
}

function VideoView({ url }: { url: string }) {
  const { src, error } = useAuthedUrl(url);
  if (error) return <Problem message={error} />;
  if (!src) return <Loading />;
  return (
    <video src={src} controls preload="metadata" style={{ width: '100%', maxHeight: '70vh', borderRadius: 8, background: '#000' }} data-testid="video-viewer">
      <track kind="captions" />
    </video>
  );
}

function CompareView({ name, urls }: { name: string; urls: { expected: string; actual: string; diff?: string } }) {
  const expected = useAuthedUrl(urls.expected);
  const actual = useAuthedUrl(urls.actual);
  const diff = useAuthedUrl(urls.diff);
  const error = expected.error ?? actual.error ?? diff.error;
  if (error) return <Problem message={error} />;
  if (!expected.src || !actual.src || (urls.diff && !diff.src)) return <Loading />;
  return <ImageCompare name={name} images={{ expected: expected.src, actual: actual.src, diff: diff.src }} />;
}

export function AttachmentBody({ project, attachment: a }: { project: string; attachment: Attachment }) {
  if (isActiveContent(a)) return <ActiveView project={project} a={a} />;
  const url = attachmentUrl(project, a.id);
  switch (attachmentKind(a)) {
    case 'image':
      return <ImageView url={url} name={a.name} />;
    case 'video':
      return <VideoView url={url} />;
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
    <CompareView
      name={entry.name}
      urls={{
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
      {list.map((a) => {
        const label = list.length > 1 ? a.name : `Download (${formatBytes(a.size)})`;
        const url = attachmentUrl(project, a.id, true);
        return usesToken() ? (
          <Button
            key={a.id}
            variant="default"
            size="xs"
            leftSection={<IconDownload size={14} />}
            onClick={() => void downloadWithToken(url, a.name).catch((e: unknown) => notifications.show({ color: 'red', title: 'Download failed', message: errorMessage(e) }))}
          >
            {label}
          </Button>
        ) : (
          <Button key={a.id} component="a" href={url} variant="default" size="xs" leftSection={<IconDownload size={14} />} download={a.name}>
            {label}
          </Button>
        );
      })}
    </Group>
  );
}
