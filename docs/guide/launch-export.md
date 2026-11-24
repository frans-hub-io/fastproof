# Exporting a launch

A launch exports as a self-contained **HTML report**, a **PDF** and an **Excel workbook**. The filters of
the launch page apply: export the failures of a launch by filtering for them first. Reading a launch is
all it takes: the permission is `launches:read`, as for the launch itself.

## From the UI

The **Export** button in the launch header offers:

| Item | What you get |
|---|---|
| HTML report | `launch-<project>-<number>.html`: one file with the launch details, the counts, a status bar and a table of the results. It works offline: no scripts, no fonts, images or other files are fetched. It follows the light or dark theme of the system |
| PDF (print the report) | The same report in a new tab (opened without access to the application; the server sends it sandboxed): press Ctrl/Cmd+P and choose **Save as PDF**. The report has a print layout (landscape A4, repeated table header, rows that do not split). Signed in with an API token, or for a launch that needs a job, the HTML file is saved and you print it from disk |
| Excel (XLSX) | A **Summary** sheet (launch, counts, the filters used) and a **Results** sheet with one row per result and a filter row |

Launches of more than 10 000 results are built by a background job: the page shows the progress and saves
the file when it is ready.

## From the command line

```sh
fastproof export --url https://fastproof.example.com --project shop --launch 0198a1c2-… \
  --format xlsx --status failed,broken --out failures.xlsx
```

`--launch` is the launch id or its external id (`FASTPROOF_LAUNCH`). `--filter 'q=checkout&newFailures=true'`
adds any filter of the result list. Launches above 10 000 results, or `--background`, use the job and wait for
it (`--timeout`, one hour by default).

`--format pdf` fetches the HTML report and prints it with a local Chromium or Chrome (the server image has
none): `--chromium /path/to/chrome`, or `FASTPROOF_CHROMIUM`, or the first of `chromium`, `chromium-browser`,
`google-chrome`, `google-chrome-stable`, `chrome` and `microsoft-edge` in `PATH`. The CLI works in a private temporary directory that it removes on every path, adds its own content security policy in front of the report (no scripts, no local files), and starts Chromium without extensions, network warm-up and scripting. Add `--no-sandbox` when
running as root in a container. Without a browser the command stops with a message; the HTML report prints
from any browser.

Use a full Chromium or Chrome: some `chrome-headless-shell` builds exit with status 0 without writing the PDF.
When the browser fails, the command prints the browser it ran, its exit status and the last lines of its error
output (D-Bus messages are left out), then how to pass a working one: `--chromium /path/to/chrome` or
`FASTPROOF_CHROMIUM`. The HTML report is always available as a fallback (`--format html`, then print it).

On a phone-sized screen the HTML report shows each result as a card (status, test, suite, duration, attempts,
triage, message) instead of a wide table; the print layout keeps the table.

## API

| Request | Purpose |
|---|---|
| `GET /api/v1/projects/{key}/launches/{id}/export?format=html\|xlsx` | The file, streamed. Up to 10 000 results (`409 export_too_large` above). `inline=true` shows the HTML in the browser instead of downloading it |
| `POST /api/v1/projects/{key}/launches/{id}/exports?format=html\|xlsx` | Starts a background job (up to 500 000 results); answers `202` with the job |
| `GET /api/v1/projects/{key}/launches/{id}/exports/{exportId}` | Status and progress (`queued`, `running`, `done`, `failed`, `expired`) |
| `GET /api/v1/projects/{key}/launches/{id}/exports/{exportId}/download` | The finished file, for 24 hours |

All of them take the filters of `GET .../results` (`status`, `q`, `suite`, `label`, `flaky`, `source`,
`message`, `retried`, `muted`, `newFailures`, `severity`, `owner`, `tag`, `minDurationMs`, `maxDurationMs`,
`defect`, `attachment`, `env`). A job is visible and downloadable only by its creator (the same token, user or static credential, not just the same name); a caller has at most
2 and a project at most 4 jobs queued or running, and the server at most 8 (`409 export_busy`, answered at once, never queued for long). A caller may create 20 exports and a project 100 per 24 hours, finished ones included (`409 export_quota`). A job for a launch of more than 500 000 results needs a filter (`409 export_too_large`). Every export is recorded in the audit
log as `launch.export`.

## What the files contain

Per result: status, full name, suite path, duration, start, attempts, flaky and muted marks, the status
change against the previous run, the defect with its group and issue, the labels and the first 500
characters of the error message. Attachments, steps and stack traces are not part of the export: follow the
launch link for them.

Test names, messages and labels come from test runs and are untrusted. In the HTML report every value is
escaped, URLs are shown as text and never as links, and the page carries a content security policy that
allows no scripts and no connections; the server also sends it with `Content-Security-Policy: sandbox`.
In the workbook text is stored as text, never as a formula, so a result named `=HYPERLINK(…)` stays text. Bidirectional and invisible format characters in names are replaced by U+FFFD in both files. If you save the workbook as CSV and open that in a spreadsheet, a text that starts with `=` would be evaluated by it: that step is outside FastProof.

## Limits and memory

Results are read in pages of 500 and written to a spool file in `FASTPROOF_SPOOL_DIR`, so memory stays flat
whatever the size; the finished job file is kept in blob storage for 24 hours and then removed (the job
`launch.exports` runs every minute; a job that makes no progress for five minutes is marked `failed`). One
background export runs per replica at a time. A file is limited to 2 GiB (`FASTPROOF_LAUNCH_EXPORT_MAX_BYTES`); a running job sends a heartbeat while it builds and uploads, files of failed jobs and of deleted projects are removed, and the export spool is swept when the server starts. Inline exports count as heavy requests.
