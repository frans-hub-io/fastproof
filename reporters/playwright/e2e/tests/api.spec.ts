import { createServer } from 'node:http';
import type { AddressInfo } from 'node:net';

import { expect, test } from '../../dist/fixture.js';

const secret = process.env.E2E_SECRET_TOKEN ?? 'no-secret';

test.describe('checkout', () => {
  test('passes with steps and output', async ({}, testInfo) => {
    testInfo.annotations.push({ type: 'owner', description: `team ${secret}` });
    await test.step('outer step', async () => {
      await test.step('inner step', async () => {
        console.log(`using token ${secret} and card-1234`);
        console.error('a warning line');
      });
    });
    await testInfo.attach(`note-${secret}`, { body: `body with ${secret}`, contentType: 'text/plain' });
    await testInfo.attach('shared.bin', { body: Buffer.alloc(2048, 1), contentType: 'application/octet-stream' });
    expect(1 + 1).toBe(2);
  });

  test('fails every time @smoke', async ({}, testInfo) => {
    await testInfo.attach('shared.bin', { body: Buffer.alloc(2048, 1), contentType: 'application/octet-stream' });
    expect(1, 'numbers differ').toBe(2);
  });

  test('flaky: passes on retry', async ({}, testInfo) => {
    console.log(`attempt ${testInfo.retry}`);
    if (testInfo.retry === 0) throw new Error('first attempt fails');
  });

  test('skipped', async () => {
    test.skip(true, 'not today');
  });
});

test('sends traceparent to the backend', async ({ request, fastproofTrace }, testInfo) => {
  const seen: Record<string, string | undefined> = {};
  const server = createServer((req, res) => {
    seen.traceparent = req.headers.traceparent as string | undefined;
    seen.baggage = req.headers.baggage as string | undefined;
    res.end('ok');
  });
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  const port = (server.address() as AddressInfo).port;
  try {
    const res = await request.get(`http://127.0.0.1:${port}/api`);
    expect(res.ok()).toBeTruthy();
  } finally {
    server.close();
  }
  expect(seen.traceparent).toBe(fastproofTrace.traceparent);
  expect(seen.baggage).toContain(`fastproof.attempt=${testInfo.retry}`);
  await testInfo.attach('seen-headers.json', { body: JSON.stringify(seen), contentType: 'application/json' });
  expect(fastproofTrace.env().TRACEPARENT).toBe(fastproofTrace.traceparent);
});
