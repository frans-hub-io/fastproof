# Importing Allure history

If you used Allure Report before FastProof, its history shows how each test behaved in earlier runs. FastProof can
bring that history in as past launches, joined to the same tests your live results use. Flaky detection, trends,
dashboards and the explorer's new failures then see what ran before the first upload.

## What is read

| Source | Files | What each past report becomes |
|---|---|---|
| Allure 2 report | `history/history.json`, and optionally `history-trend.json`, `duration-trend.json`, `categories-trend.json`, `retry-trend.json` | One finished launch per past report, with the status, time and duration of each test in it; the trend files add the totals, duration, retries and categories of the whole report as launch attributes (`history.report.*`, `history.category.*`) |
| Allure 3 | The history file (`historyPath`, one JSON line per report) | One finished launch per line, with each test's status, time, retries, environment and failure message, and the report's metrics as attributes (`history.metric.*`) |

Every imported launch is dated at the first result of its report, is not a debug launch and carries the attribute
`imported=allure-history`, so it can be found in the launch list. It does not send notifications, evaluate gates or
queue case sync, and emits no analytics events. History feeds statistics only: the status string behind the flaky list and the flaky score, the first and last seen times of tests, and the baselines that later live runs compare with (transitions, new failures, flaky). Imported results themselves get no defect rules, mutes, default defects, fingerprints or transitions, so old failures never mute a test or fill the triage queue.

## Joining the tests

The Allure `historyId` of a test is the FastProof series key (`md5(testCaseId or fullName)`, a dot, `md5(parameters)`,
and for Allure 3 a third part for the environment). So history lands on the series live results already use, with
no mapping.

- **Allure 2** history files carry no test names. A result is stored only when the project already knows its test (the first live upload
  of the same tests); a known test gets a series of its own for the environment you give the import. The others are counted as *unknown tests* in the import
  summary. Import after the first live run.
- **Allure 3** history names every test, so tests the project does not know yet are created with their names; live
  results join them later.

Allure 2 keys have no environment. If your live runs set an environment (`--environment`, `FASTPROOF_ENVIRONMENT`),
give the same one to the import, or the series will not match.

## Branch and environment

Flaky detection and new-failure baselines read the history of one branch. Give the import the branch your live runs
report (`--branch main`); without it the imported launches have no branch and are only seen by launches without one.

## Allure 2: how past reports are put together

`history.json` lists, per test, its last runs, newest first, and does not say which report each run belonged to.
FastProof puts the n-th newest run of every test into the n-th newest report, which holds while every test runs in
every report. When runs carry a `reportUrl` (Jenkins and similar), runs are grouped by it instead. If tests were added,
removed or sharded between reports, the alignment can mix reports; the import warns when a group's runs started more than
three days apart. Allure keeps at most 20 runs per test, so an Allure 2 import brings at most 20 launches.

## From the command line

```sh
fastproof import allure-history path/to/allure-report/history --branch main
fastproof import allure-history path/to/history.jsonl --branch main --environment staging
```

The path is a directory with `history.json` (or `history/history.json`; the four trend files next to it are sent too),
a `history.json`, or an Allure 3 history file. `--source allure2|allure3` overrides the detection from the files.
The command waits for the import and prints how many reports, launches and results it stored (`--no-wait` prints
the import id and returns; `--timeout` seconds, default 3600). The exit code is `0` when the import is done, `1`
when it failed and `2` when it was still running at the timeout. It needs the same variables as `upload`
(`FASTPROOF_URL`, `FASTPROOF_TOKEN`, `FASTPROOF_PROJECT`) and the `settings:write` permission: an import backdates launches and rewrites statistics, so a CI token that only uploads results cannot do it.

## From the UI

Project settings, **History import** tab: choose the source, the files, the branch and the environment. The list below
shows the import while it runs and keeps the last twenty with their counters and notes.

## Safe to repeat

An import of the same files adds nothing (each chunk of results is stored together with its statistics in one transaction, so an import that failed midway is simply run again): a past report is identified by its Allure 3 report id, its report URL or
the start time of its first result (Allure 2), plus the branch and environment of the import. Run it again after a
newer report adds a run: only the new report becomes a launch, and the runs of known reports that were missing are
added. Different branches are different histories. A launch whose derived id is held by a launch that no import of the same caller made (a live launch, another user's) is never written into: the import takes the next derived id.

## Counters

| Counter | Meaning |
|---|---|
| reports | Past reports found and processed |
| launches created | Past launches made now (not those of an earlier import) |
| results stored | Results written |
| already there | Results an earlier import stored |
| unknown tests | Allure 2 results of tests the project does not know |
| left out | Results of reports older than the project's retention window (they would be deleted at once), more than a day in the future, or without a usable time or key |

Raise `retention.rawDays` in the project settings before importing older history.

## Limits

Fixed defaults, answered with `413 payload_too_large` or a failed import naming the limit.

| Limit | Value |
|---|---|
| `history.json` | 32 MiB |
| Each trend file | 4 MiB |
| Allure 3 history file | 256 MiB, one line at most 32 MiB, 1000 reports |
| Tests in `history.json` | 200 000, 1000 runs per test |
| Results in one import | 500 000; 100 000 in one report |
| Earlier attempts kept per result | 5; at most 1 000 000 attempts in one import |
| Age of a report | at most 10 years, and inside the project's retention window |
| Time of a result | start, stop and start plus duration must lie in that window and not more than 5 minutes ahead of the server clock, else the result is left out |
| Failure message kept | 1 KiB; test names 4 KiB |
| JSON | depth 32, one string 1 MiB, 5 000 000 values per document or line |
| Imports | one running per project and per caller, four on the server; 20 per project per 24 hours; one import runs at most 3 hours; the caps are checked before the files are read |
| Upload size | each file also stays within `FASTPROOF_MAX_UPLOAD_BYTES` |

The files are parsed with bounded memory: `history.json` is read by a visitor that stops at the caps, and an
Allure 3 file is read line by line, one report at a time. Lines that are not valid reports are skipped with a
warning. An import whose files hold no report fails with a message.

## API

`POST /api/v1/projects/{key}/history-imports?source=allure2|allure3&branch=&environment=` with `multipart/form-data`
fields `history` (required), and for Allure 2 `historyTrend`, `durationTrend`, `categoriesTrend`, `retryTrend`
answers `202` with the import; `GET .../history-imports` lists the last twenty and `GET .../history-imports/{id}`
reads one. Writing needs `settings:write`, reading `launches:read`. The creation and the end of an import are audited as `history.import`, `history.import.done` and `history.import.failed`.
Errors: `409 history_import_busy`, `409 history_import_quota`.
