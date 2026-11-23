import { chromium } from '@playwright/test';
import { mkdtempSync, readdirSync, copyFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const out = resolve(process.argv[2] ?? 'e2e/fixtures/sample.webm');
const seconds = Number(process.argv[3] ?? '12');
const dir = mkdtempSync(join(tmpdir(), 'fp-video-'));
const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 320, height: 200 }, recordVideo: { dir, size: { width: 320, height: 200 } } });
const page = await context.newPage();
await page.setContent(`<body style="margin:0;font:700 40px monospace;background:#12313f;color:#fff">
<div id="t" style="padding:24px 16px"></div><div id="b" style="height:24px;background:#3fb68b;width:0"></div>
<script>const s=performance.now();setInterval(()=>{const e=(performance.now()-s)/1000;
document.getElementById('t').textContent='t = '+e.toFixed(1)+' s';
document.getElementById('b').style.width=(e*26)%320+'px';
document.body.style.background='hsl('+(e*30)%360+',45%,22%)'},100)</script></body>`);
await page.waitForTimeout(seconds * 1000);
await context.close();
await browser.close();
const file = readdirSync(dir).find((f) => f.endsWith('.webm'));
copyFileSync(join(dir, file), out);
rmSync(dir, { recursive: true, force: true });
