# Exporting a launch

A launch exports as a self-contained **HTML report**, a **PDF** and an **Excel workbook**. The filters of
the launch page apply: export the failures of a launch by filtering for them first. Reading a launch is
all it takes: the permission is `launches:read`, as for the launch itself.

## From the UI

The **Export** button in the launch header offers:

| Item | What you get |
|---|---|
| HTML report | `launch-<project>-<number>.html`: one file with the launch details, the counts, a status bar and a table of the results. It works offline: no scripts, no fonts, images or other files are fetched. It follows the light or dark theme of the system |
| PDF (print the report) | The same report in a new tab with the print dialog open: choose **Save as PDF**. The report has a print layout (landscape A4, repeated table header, rows that do not split) |
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
`google-chrome`, `google-chrome-stable`, `chrome` and `microsoft-edge` in `PATH`. Add `--no-sandbox` when
running as root in a container. Without a browser the command stops with a message; the HTML report prints
from any browser.

## API

| Request | Purpose |
|---|---|
| `GET /api/v1/projects/{key}/launches/{id}/export?format=html\|xlsx` | The file, streamed. Up to 10 000 results (`409 export_too_large` above). `inline=true` shows the HTML in the browser instead of downloading it |
| `POST /api/v1/projects/{key}/launches/{id}/exports?format=html\|xlsx` | Starts a background job (up to 500 000 results); answers `202` with the job |
| `GET /api/v1/projects/{key}/launches/{id}/exports/{exportId}` | Status and progress (`queued`, `running`, `done`, `failed`, `expired`) |
| `GET /api/v1/projects/{key}/launches/{id}/exports/{exportId}/download` | The finished file, for 24 hours |

All of them take the filters of `GET .../results` (`status`, `q`, `suite`, `label`, `flaky`, `source`,
`message`, `retried`, `muted`, `newFailures`, `severity`, `owner`, `tag`, `minDurationMs`, `maxDurationMs`,
`defect`, `attachment`, `env`). A job is visible and downloadable only by its creator; a caller has at most
2 and a project at most 4 jobs queued or running (`409 export_busy`). Every export is recorded in the audit
log as `launch.export`.

## What the files contain

Per result: status, full name, suite path, duration, start, attempts, flaky and muted marks, the status
change against the previous run, the defect with its group and issue, the labels and the first 500
characters of the error message. Attachments, steps and stack traces are not part of the export: follow the
launch link for them.

Test names, messages and labels come from test runs and are untrusted. In the HTML report every value is
escaped, URLs are shown as text and never as links, and the page carries a content security policy that
allows no scripts and no connections; the server also sends it with `Content-Security-Policy: sandbox`.
In the workbook text is stored as text, never as a formula, so a result named `=HYPERLINK(…)` stays text.

## Limits and memory

Results are read in pages of 500 and written to a spool file in `FASTPROOF_SPOOL_DIR`, so memory stays flat
whatever the size; the finished job file is kept in blob storage for 24 hours and then removed (the job
`launch.exports` runs every minute; a job that makes no progress for five minutes is marked `failed`). One
background export runs per replica at a time. Inline exports count as heavy requests.
