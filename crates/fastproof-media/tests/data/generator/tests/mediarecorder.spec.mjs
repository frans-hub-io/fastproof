import { test } from "@playwright/test";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { CLOCK } from "./clock.mjs";

test.use({ video: "off" });

test("canvas recorded with MediaRecorder", async ({ page }, testInfo) => {
  await page.setContent(CLOCK);
  const b64 = await page.evaluate(async () => {
    const stream = document.getElementById("c").captureStream(25);
    const rec = new MediaRecorder(stream, { mimeType: "video/webm;codecs=vp8", videoBitsPerSecond: 400000 });
    const parts = [];
    rec.ondataavailable = (e) => parts.push(e.data);
    const done = new Promise((r) => (rec.onstop = r));
    rec.start(1000);
    await new Promise((r) => setTimeout(r, 6000));
    rec.stop();
    await done;
    const buf = new Uint8Array(await new Blob(parts).arrayBuffer());
    let s = "";
    for (let i = 0; i < buf.length; i += 0x8000) s += String.fromCharCode(...buf.subarray(i, i + 0x8000));
    return btoa(s);
  });
  const path = join(await mkdtemp(join(tmpdir(), "recording-")), "mediarecorder.webm");
  await writeFile(path, Buffer.from(b64, "base64"));
  await testInfo.attach("recording", { path, contentType: "video/webm" });
});
