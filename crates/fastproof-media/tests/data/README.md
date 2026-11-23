# Media fixtures

Generated, not handwritten. Do not edit by hand; rerun the generator.

- Tools: Node 22, `@playwright/test` 1.65.0 with its Chromium headless shell and ffmpeg build.
- Command: `tests/data/generator/generate.sh` (optional `WORK=<dir>` and `npm_config_cache=<dir>`). Paths in the
  report are rewritten to `/builds/example/fastproof-samples`.
- Two tests at 320x240 drawing a running clock on a canvas.

| File | Contents |
| --- | --- |
| `playwright/results.json` | built-in `json` reporter of the run |
| `playwright/test-results/screen-screen-recording/video.webm` | Playwright's own screen recording (`video: "on"`, about 6 s, VP8, written by ffmpeg): `Duration`, a `SeekHead` and `Cues` after the clusters. Already seekable. |
| `playwright/test-results/mediarecorder-canvas-recorded-with-MediaRecorder/attachments/recording-*.webm` | the canvas recorded for 6 s with the browser's `MediaRecorder` (VP8) and attached with `testInfo.attach`: no `Duration`, no `Cues`, a Segment and Clusters of unknown size, `BlockGroup`s with `BlockAdditions`. |

The tests also build synthetic and corrupted files from these at run time (`tests/common/mod.rs`).
