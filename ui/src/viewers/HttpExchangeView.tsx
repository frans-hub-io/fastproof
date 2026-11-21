import { Badge, Code, Group, Stack, Table, Tabs, Text } from '@mantine/core';
import type { HttpExchange, HttpExchangeBody, HttpExchangeHeader } from '../api/types';
import { formatBytes, formatDuration } from '../lib/format';
import { JsonTree } from './JsonTree';

const REDACTED = '__ALLURE_REDACTED__';

function Headers({ list, title }: { list?: HttpExchangeHeader[]; title: string }) {
  if (!list?.length) return null;
  return (
    <Stack gap={4}>
      <Text size="xs" fw={600} c="dimmed">
        {title}
      </Text>
      <div className="fp-table-wrap">
        <Table withTableBorder withColumnBorders fz="xs" verticalSpacing={4}>
          <Table.Tbody>
            {list.map((h, i) => (
              <Table.Tr key={`${h.name}-${String(i)}`}>
                <Table.Td className="fp-mono" style={{ whiteSpace: 'nowrap', width: '30%' }}>
                  {h.name}
                </Table.Td>
                <Table.Td className="fp-mono" style={{ wordBreak: 'break-all' }}>
                  {h.value === REDACTED ? (
                    <Badge size="xs" variant="light" color="gray">
                      redacted
                    </Badge>
                  ) : (
                    h.value
                  )}
                </Table.Td>
              </Table.Tr>
            ))}
          </Table.Tbody>
        </Table>
      </div>
    </Stack>
  );
}

function decode(body: HttpExchangeBody): string | null {
  if (body.value === undefined) return null;
  if (body.encoding === 'base64') {
    try {
      const bin = atob(body.value);
      const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
      return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
    } catch {
      return null;
    }
  }
  return body.value;
}

function Body({ body, headers }: { body?: HttpExchangeBody | null; headers?: HttpExchangeHeader[] }) {
  if (!body || body.value === undefined || body.value === '') return null;
  const text = decode(body);
  const ct = body.contentType ?? headers?.find((h) => h.name.toLowerCase() === 'content-type')?.value ?? '';
  let json: unknown;
  let isJson = false;
  if (text !== null && (/json/i.test(ct) || /^\s*[[{]/.test(text))) {
    try {
      json = JSON.parse(text);
      isJson = true;
    } catch {
      isJson = false;
    }
  }
  return (
    <Stack gap={4}>
      <Text size="xs" fw={600} c="dimmed">
        Body {body.size !== undefined ? `(${formatBytes(body.size)})` : ''}
      </Text>
      {text === null ? (
        <Text size="xs" c="dimmed">
          Binary body ({body.encoding ?? 'unknown encoding'}).
        </Text>
      ) : isJson ? (
        <JsonTree value={json} />
      ) : (
        <pre className="fp-log">{text}</pre>
      )}
    </Stack>
  );
}

function statusColor(s: number | undefined): string {
  if (!s) return 'gray';
  if (s < 300) return 'green';
  if (s < 400) return 'blue';
  if (s < 500) return 'orange';
  return 'red';
}

export function HttpExchangeView({ exchange }: { exchange: HttpExchange }) {
  const req = exchange.request;
  const res = exchange.response;
  const took = exchange.start !== undefined && exchange.stop !== undefined ? exchange.stop - exchange.start : null;
  return (
    <Stack gap="sm" data-testid="http-exchange">
      <Group gap="xs" wrap="wrap">
        <Badge variant="filled" color="fhPurple" radius="sm">
          {req?.method ?? '?'}
        </Badge>
        <Code style={{ wordBreak: 'break-all', whiteSpace: 'normal' }}>{req?.url ?? ''}</Code>
        {res?.status !== undefined && (
          <Badge variant="light" color={statusColor(res.status)} radius="sm">
            {String(res.status)} {res.statusText ?? ''}
          </Badge>
        )}
        {took !== null && (
          <Text size="xs" c="dimmed">
            {formatDuration(took)}
          </Text>
        )}
      </Group>
      <Tabs defaultValue="response" keepMounted={false}>
        <Tabs.List>
          <Tabs.Tab value="request">Request</Tabs.Tab>
          <Tabs.Tab value="response">Response</Tabs.Tab>
        </Tabs.List>
        <Tabs.Panel value="request" pt="sm">
          <Stack gap="sm">
            <Headers list={req?.headers} title="Headers" />
            <Headers list={req?.cookies} title="Cookies" />
            <Body body={req?.body} headers={req?.headers} />
          </Stack>
        </Tabs.Panel>
        <Tabs.Panel value="response" pt="sm">
          <Stack gap="sm">
            <Headers list={res?.headers} title="Headers" />
            <Headers list={res?.cookies} title="Cookies" />
            <Body body={res?.body} headers={res?.headers} />
          </Stack>
        </Tabs.Panel>
      </Tabs>
    </Stack>
  );
}
