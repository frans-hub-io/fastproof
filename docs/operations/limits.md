# Limits

Every upload is parsed with size, depth and count limits, so a malformed or hostile report cannot exhaust
memory or disk. One byte budget per upload is charged for every expansion: each byte read from an archive
entry after decompression, inline attachment bodies and fixture copies. That also stops zip bombs and nested
archives early. Sizes declared in archive headers are never trusted.

When a limit is hit, the upload fails with an error that names the limit and its value, for example
`limit max_entry_bytes exceeded (max 536870912): allure-results/video.webm`. Truncation limits (output,
messages and names) do not fail the upload: the text is cut and, for names, the upload lists a warning.

Only `max_upload_bytes` is a server setting; the other upload limits are fixed defaults.

## Defaults

| Limit | Default | Applies to |
|---|---|---|
| `max_upload_bytes` | 1 GiB | Size of the upload as received (archive or single file); set with `FASTPROOF_MAX_UPLOAD_BYTES` |
| `max_archive_entries` | 100 000 | Entries in an archive or files in a directory |
| `max_total_uncompressed_bytes` | 4 GiB | Byte budget of one upload across all expansions |
| `max_entry_bytes` | 512 MiB | One archive entry after decompression |
| `max_document_bytes` | 64 MiB | One report document (JSON or XML) parsed in memory |
| `max_path_bytes` | 1024 | Length of an entry path inside an archive |
| `max_json_depth` | 128 | Nesting depth of JSON documents |
| `max_xml_depth` | 64 | Nesting depth of XML documents |
| `max_xml_attributes` | 64 | Attributes per XML element |
| `max_string_bytes` | 8 MiB | One JSON string or XML text or attribute value |
| `max_json_values` | 5 000 000 | JSON values (scalars, arrays, objects) in one document, checked before it is parsed |
| `max_zip_directory_bytes` | 64 MiB | Size of a zip central directory, declared or read while the archive is opened |
| `max_log_bytes` | 1 MiB | Kept size of stdout, stderr, message and trace per result; longer text is truncated |
| `max_name_bytes` | 4 KiB | Kept length of a test name and full name, in uploads and in `POST .../results`; longer names are truncated with a warning (`warnings` in the answer of `POST .../results`) |
| writer-side result id (Allure `uuid`, CTRF `id`) | 256 B kept as is | A longer id is replaced by its SHA-256 in the stored attempt key; the same id still makes the same attempt, also when an earlier version stored it whole (up to 64 KiB) |
| failure message, for search | first 64 Ki characters | Fixed. The message is kept up to `max_log_bytes`, but the text search of results (`q`, the message filter) reads only its first 64 Ki characters |
| label name, label value | 256 B, 1 KiB | Fixed, not configurable: longer label names and values are cut on a character boundary and end with an ellipsis, with a warning |
| `max_steps_per_result` | 10 000 | Steps per result, counted across nesting, fixtures included |
| `max_labels_per_result` | 500 | Labels (tags, properties, annotations) per result |
| `max_attempts_per_result` | 100 | Attempts (retries included) of one test in one report |
| `max_attachments_per_result` | 1000 | Attachment references per result, steps included |
| `max_attachments` | 200 000 | Attachment references per upload: every reference counts, also several references to the same file |
| `max_inline_attachment_bytes` | 256 MiB | Decoded inline attachment bodies (Playwright `body`) held in memory per upload |
| `max_results` | 200 000 | Results per upload; each attempt counts |

## Results API and live logs

Results and log lines sent by reporters (`POST .../results`, `POST .../logs`) have their own limits. A request
over a limit is refused with `413` and a message that names the limit with its unit, for example
`the body exceeds max_lines_per_request (max 5000 lines)`; `details` holds `limit` and `max`.

| Limit | Default | Server setting |
|---|---|---|
| Results per request | 200 000 | same as `max_results` |
| Log message of one line | 64 KiB, longer messages are cut and end with ` …[truncated]` | `FASTPROOF_LOG_MAX_LINE_BYTES` |
| Log lines per request | 5000 | `FASTPROOF_LOG_MAX_LINES_PER_REQUEST` |
| Log request body | 8 MiB | `FASTPROOF_LOG_MAX_REQUEST_BYTES` |
| Log bytes per launch | 256 MiB | `FASTPROOF_LOG_MAX_BYTES_PER_LAUNCH` |

See [Live logs](../guide/live-logs.md) for the log fields.

## ReportPortal endpoint

Limits of `/rp/api/{v1,v2}` (see [Migrating from ReportPortal](../guide/migrating-from-reportportal.md)).

| Limit | Default | Setting or behaviour |
|---|---|---|
| JSON body of a launch, item or single log call | 1 MiB | Fixed; `413` |
| Body of a multipart log request | 64 MiB | `FASTPROOF_RP_MAX_REQUEST_BYTES`; `413` |
| One file of a log request | 64 MiB | `FASTPROOF_RP_MAX_ATTACHMENT_BYTES`; only that entry fails |
| Files attached to one item | 1000 | Fixed (the native per-result limit); further entries fail, the log line is kept |
| `json_request_part` | 8 MiB, 1000 log entries | Fixed; `413` |
| Parts of a multipart request | 1001 | Fixed; `413` |
| Header of one part | 8 KiB | Fixed; `400` |
| Items per launch | 1 000 000 | `FASTPROOF_RP_MAX_ITEMS_PER_LAUNCH`; counted every 100 items per replica and on every item once fewer than 100 are left, then `413` for every further item (finishes and log lines still pass) |
| Depth of the item tree | 64 | Fixed; `400` |
| Steps per result | 10 000 | Fixed; more are dropped with a warning in the server log |
| Attributes and parameters per item | 100 each, keys 256 B, values 1 KiB | Fixed; the rest is dropped or cut |
| Names | 4 KiB | Cut |
| Container names above a test (suite labels, full name without `codeRef`) | 1 KiB each, 4 KiB in all | Fixed; deeper containers are left out |
| Client uuids | 64 B of letters, digits, `.`, `_`, `-` | `400` |
| Times | 1970 to 5 minutes from now | Clamped |
| Requests at once per replica | 256 | `FASTPROOF_RP_MAX_INFLIGHT`; `503` with `Retry-After: 1` after a 2 s wait |
| A child whose parent did not arrive | 2 minutes | Then it becomes a root item |
| A failed test waiting for its error message | 30 s after its finish and after the launch finish | `FASTPROOF_RP_ERROR_WAIT_MS`; then it is written without one. The message is the first line of the last ERROR or FATAL line (4 KiB), the trace its whole text (1 MiB) |
| Working set of a launch | a day after it finished | Item rows are deleted; the results stay |

Log lines follow the [live log limits](#results-api-and-live-logs). `Content-Encoding` other than `identity`
gets `415`. A request without a valid token is answered `401` before its body is read and takes no slot of
`FASTPROOF_RP_MAX_INFLIGHT`.

## Allure history import

`POST .../history-imports` and `fastproof import allure-history` have their own fixed limits: `history.json` 32 MiB,
each trend file 4 MiB, an Allure 3 history file 256 MiB (a line 32 MiB, 1000 reports), 200 000 tests in
`history.json`, 500 000 results and 1 000 000 attempts per import, reports at most 10 years old, one running import per project and per caller, four on the server. Each file also respects `max_upload_bytes`. See
[Importing Allure history](../guide/importing-allure-history.md#limits).

## API limits

| Limit | Value | When exceeded |
|---|---|---|
| One NDJSON line of `POST .../results` | 16 MiB (and at most `max_document_bytes`) | 413 `max_document_bytes` |
| JSON values per results or logs request | `max_json_values` (5,000,000) for the whole request, NDJSON lines summed | 413 `max_json_values` |
| Dashboard days | years 1970 to 9999, window at most 3660 days | 400 |
| Log search window | default 7 days before `to`, at most 31 days | 400 |
| `q` and text filters of result and launch lists | 256 bytes | 400 |
| Test plans made from launches or test ids (`fastproof testplan --from-launch`) per project | 1000; deleted 90 days after creation. [Managed plans](../guide/manual-runs.md) do not count and do not expire | 429 |
| Stored log chunk | 1000 lines and 1 MiB | — |
| Live log subscriber queue | 2000 lines and 1 MiB; older lines are dropped and reported as `gap` | a subscriber that reads nothing for 60 s while its queue overflows is disconnected |
| ClickHouse search query | 30 s, 2 GiB memory | 503 |
| OpenSearch search | 30 s | 503 |
| SQL statement of a heavy read | `FASTPROOF_DATABASE_STATEMENT_TIMEOUT_SECONDS` | 503 `timeout` |
| Tree groups of one result with several values of a grouping label | 20 values per label, 64 groups | further values and combinations are left out |
| Groups of one launch tree, and distinct placements it reads | 200 000 each | `partial: true` on the root; the counts cover the groups listed |
| Results read for the label names and filter counts of a launch | 200 000 (and 64 values per label name of a result for the filter counts) | `partial: true` |
| Cached explorer aggregates | 32 per launch, 2 MiB each, kept 7 days | the oldest go; larger ones are not cached |

The launch tree, its pages, the result list, the label names, the filter counts and the environments of a launch
are heavy requests (`FASTPROOF_HEAVY_QUERIES_PER_CALLER`).
| [Direct uploads](../guide/direct-uploads.md) per request | 1 to 100 attachments | 400 |
| Direct upload size, open uploads per launch and project, declared bytes per launch and of open uploads per project | `FASTPROOF_DIRECT_UPLOAD_MAX_BYTES` (1 GiB, at most 5 GiB and never above the server path's limits), `_MAX_OPEN_PER_LAUNCH` (1000), `_MAX_OPEN_PER_PROJECT` (10000), `_MAX_BYTES_PER_LAUNCH` (50 GiB), `_MAX_OPEN_BYTES_PER_PROJECT` (100 GiB) | the item is `refused` with the limit; the client sends it through the server |
| Direct upload verifications per replica | `FASTPROOF_DIRECT_UPLOAD_VERIFY_CONCURRENCY` (4), each at most `_VERIFY_TIMEOUT_SECONDS` (600) | 503 with `Retry-After` |

## Test management

[Test cases](../guide/test-management.md), [plans and manual runs](../guide/manual-runs.md) and
[imports](../guide/importing-test-cases.md) have limits of their own. Each is a server setting
`FASTPROOF_TM_<NAME>`; a value outside `1` to the maximum stops the server at start with a message naming
the variable. Counts answer `409 limit_reached` with `details` `{"limit", "max"}` (for example
`{"limit": "maxFolders", "max": 20000}`), sizes of uploaded files `413 payload_too_large`, Markdown fields
over their size `400 invalid_markdown` with the field's `path`, and names `400`.

| Variable | Default | Maximum | Limit |
|---|---|---|---|
| `FASTPROOF_TM_MAX_STEPS` | `500` | 10 000 | Steps of one case, all levels counted |
| `FASTPROOF_TM_MAX_STEP_DEPTH` | `3` | 10 | Nesting levels of steps |
| `FASTPROOF_TM_MAX_MARKDOWN_BYTES` | `16384` (16 KiB) | 1 MiB | One Markdown field: description, preconditions, a step's action, expected result or data, a run comment or actual result |
| `FASTPROOF_TM_MAX_BODY_BYTES` | `1048576` (1 MiB) | 16 MiB | The whole content of a case |
| `FASTPROOF_TM_MAX_ATTACHMENT_BYTES` | `52428800` (50 MiB) | 4 GiB | One attachment of a case or of a manual step result |
| `FASTPROOF_TM_MAX_CASE_ATTACHMENTS` | `100` | 10 000 | Attachments a case has had, over all its versions |
| `FASTPROOF_TM_MAX_STEP_ATTACHMENTS` | `20` | 1000 | Attachments of one step result in a manual run, and of one step of a case |
| `FASTPROOF_TM_MAX_FIELDS` | `100` | 1000 | Custom fields per project, built-in ones included |
| `FASTPROOF_TM_MAX_FIELD_OPTIONS` | `1000` | 10 000 | Options of one select or multiselect field |
| `FASTPROOF_TM_MAX_FOLDERS` | `20000` | 1 000 000 | Folders per project |
| `FASTPROOF_TM_MAX_FOLDER_DEPTH` | `10` | 100 | Folder levels (the top level is 1) |
| `FASTPROOF_TM_MAX_CASES` | `200000` | 10 000 000 | Cases per project, the trash included |
| `FASTPROOF_TM_MAX_PLAN_CASES` | `50000` | 1 000 000 | Cases of one test plan |
| `FASTPROOF_TM_MAX_RUN_ITEMS` | `50000` | 1 000 000 | Run items (manual cases) of one launch |
| `FASTPROOF_TM_MAX_BULK` | `10000` | 100 000 | Cases of one bulk change, plan change or run item request |
| `FASTPROOF_TM_MAX_IMPORT_BYTES` | `104857600` (100 MiB) | 16 GiB | An imported CSV or JSON file |
| `FASTPROOF_TM_MAX_IMPORT_ZIP_BYTES` | `2147483648` (2 GiB) | 64 GiB | Byte budget of the attachments zip of an import, after decompression |
| `FASTPROOF_TM_MAX_IMPORT_ROWS` | `200000` | 10 000 000 | Rows of an imported file |
| `FASTPROOF_TM_MAX_IMPORT_CELL_BYTES` | `65536` (64 KiB) | 16 MiB | One cell of an imported CSV file |
| `FASTPROOF_TM_MAX_TAGS` | `50` | 1000 | Tags of one case (each at most 100 bytes) |
| `FASTPROOF_TM_MAX_NAME_BYTES` | `500` | 4096 | Names of cases, folders, fields and plans |
| `FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE` | `30` | 100 000 | Writes that multiply work, per caller and minute on one replica: starting a plan launch, adding run items, changing plan items, plan sync, creating a plan with a filter, bulk case changes, deleting a folder with a `mode`, and every MCP write tool. Over it they answer `429 rate_limited` with `Retry-After` |
| `FASTPROOF_TM_MAX_OPEN_PLAN_LAUNCHES` | `20` | 100 000 | Launches with an open manual run that one caller started from plans, per project; one more, or reopening a closed one, answers `409 limit_reached` (`maxOpenPlanLaunches`) until one is closed |
| `FASTPROOF_TM_MAX_SHARED_STEPS` | `5000` | 1 000 000 | [Shared steps](../guide/shared-steps.md) per project, archived ones included (`maxSharedSteps`) |
| `FASTPROOF_TM_MAX_SHARED_REFS` | `50` | 1000 | `shared` steps in one case (`maxSharedRefs`) |
| `FASTPROOF_TM_MAX_SHARED_STEP_STEPS` | `100` | 10 000 | Steps of one shared step, all levels (depth at most 2) |
| `FASTPROOF_TM_MAX_SHARED_STEP_BYTES` | `65536` (64 KiB) | 16 MiB | The steps of one shared step as JSON |
| `FASTPROOF_TM_MAX_PARAMETER_COLUMNS` | `20` | 1000 | Columns of the [parameters](../guide/parameters.md) of a case (`maxParameterColumns`) |
| `FASTPROOF_TM_MAX_PARAMETER_ROWS` | `1000` | 100 000 | Rows of the parameters of a case, and rows a generator may produce (`maxParameterRows`) |
| `FASTPROOF_TM_MAX_PARAMETER_VALUE_BYTES` | `1024` (1 KiB) | 1 MiB | One parameter value |
| `FASTPROOF_TM_MAX_RUN_ITERATIONS` | `50000` | 10 000 000 | Iterations of one launch, all parameterized run items together (`maxRunIterations`); a launch, run items or a rerun that would pass it answers `409 limit_reached` before anything is written |
| `FASTPROOF_TM_MAX_REQUIREMENTS` | `50000` | 10 000 000 | [Requirements](../guide/requirements.md) per project, archived ones included (`maxRequirements`) |
| `FASTPROOF_TM_MAX_REQUIREMENT_DEPTH` | `6` | 100 | Levels of the requirement tree (the top level is 1) |
| `FASTPROOF_TM_MAX_REQUIREMENT_CASES` | `500` | 1 000 000 | Links of one requirement to cases, proposed ones included (`maxRequirementCases`) |
| `FASTPROOF_TM_MAX_SAVED_QUERIES` | `100` | 100 000 | [Saved queries](../guide/query-language.md#saved-queries) per owner and entity; shared ones are limited to twice this per project |
| `FASTPROOF_TM_MAX_BULK_REVIEW` | `500` | 100 000 | Versions in one bulk review (`POST .../reviews/bulk`) |
| `FASTPROOF_TM_GATE` | `auto` | | `auto`, `open` or `closed`: when new data formats may be written while replicas of an older version run (see [Upgrading](upgrading.md#next-release-test-management-gate-and-review)) |
| `FASTPROOF_TM_GATE_SETTLE_SECONDS` | `600` | 2 592 000 | How long after the upgrade migration the `auto` gate waits for replicas that wrote no level |

Fixed caps that are not settings:

| Cap | Value | Answer |
|---|---|---|
| Launch export jobs (HTML, XLSX) queued or running | 2 per caller, 4 per project, 8 per server | `409 export_busy` |
| Launch exports created per 24 hours | 20 per caller, 100 per project | `409 export_quota` |
| Launch export file | 2 GiB (`FASTPROOF_LAUNCH_EXPORT_MAX_BYTES`) | the job fails |
| Results of an inline launch export / of a launch export job | 10 000 / 500 000 | `409 export_too_large` (inline), job `failed` |
| Zip export jobs queued or running | 2 per caller and project, 4 per project | `409 export_busy` (`details.limit`: `exportsPerCaller` or `exportsPerProject`) |
| Pending AI suggestions per case | 20 | `409 limit_reached` (`maxPendingSuggestions`) |
| Unreviewed AI draft cases per principal and project | 200 | `409 limit_reached` (`maxUnreviewedAiDrafts`) |
| Unreviewed AI draft shared steps per principal and project | 200 | `409 limit_reached` (`maxUnreviewedAiDrafts`) |
| Draft requirements made by one AI principal per project (`origin ai`, `draft`, not archived) | 200 | `409 limit_reached` (`maxAiDraftRequirements`) |
| Open link proposals (`status proposed`) of one requirement | 50, and they count toward `FASTPROOF_TM_MAX_REQUIREMENT_CASES` | `409 limit_reached` (`maxProposedLinks`) |
| Open link proposals of one AI principal per project, over all requirements | 1000 | `409 limit_reached` (`maxAiProposedLinks`) |
| Cases per `propose_requirement_links` call, requirements per `draft_requirements_from_text` call | 20 each | tool error |
| Requirements a case lists, requirement keys one result names | 1000, 50 | the rest is left out |
| Coverage `launchIds`, window days | 50, 365 | `400` |
| Coverage exports queued or running | 2 per caller, 4 per project | `409 export_busy` |
| Tracker import of requirements: issues, query, text, dry runs kept waiting | 1000, 1000 bytes, 8 MiB, 3 per project | the job fails or the oldest dry run is cancelled |
| Disabled rules per principal and project before `create_rule_draft` refuses | 20 | tool error `limit_reached` (`maxRuleDrafts`) |
| One line of a Playwright trace read by `trace_actions` | 1 MiB | longer lines are skipped |

FPQL has fixed limits of its own (query 4096 bytes, 64 conditions, 8 levels, 1000 values in a list, 5 s per
statement; see [Query language](../guide/query-language.md#limits)). A coverage page with a scope, a requirement search,
`validate_query`, a case list with a query, a bulk review and an inline of a shared step take the per-caller
heavy slots (`FASTPROOF_HEAVY_QUERIES_PER_CALLER`). Votes, transitions, bulk reviews, dispatches, inlines, parameter
generation, requirement links and the MCP writes take the per-caller write rate.

Zip exports run in worker slots of their own, so a queue of exports never delays imports. Zip export
creation, `trace_actions`, starting a plan launch, adding run items, changing plan items, plan sync, creating
a plan with a filter and deleting a folder with a `mode` use the per-caller heavy slots
(`FASTPROOF_HEAVY_QUERIES_PER_CALLER`; over it they answer `429`). Each change of a case stores a compressed
snapshot (a bulk change writes a version only for the cases it actually changes). Versions are kept for the
life of the case unless the project sets a [version retention](../guide/test-management.md#versions)
(`retention.caseVersionsKeep`, `retention.caseVersionsDays`); without one, plan storage for them like for the
cases themselves.

`FASTPROOF_TM_BACKFILL_DELAY` (default `10m`, at most `30d`; `0`, seconds, or a number with `s`, `m`, `h` or
`d`) is how long after the upgrade the [backfill of cases](../guide/test-management.md#after-an-upgrade-cases-for-existing-tests)
waits before it decides per project; projects with code sync off by then are skipped.

How an import answers when it hits a limit:

| Limit | Answer |
|---|---|
| `FASTPROOF_TM_MAX_IMPORT_BYTES`, `FASTPROOF_TM_MAX_IMPORT_ZIP_BYTES` (upload) | `413 payload_too_large` when the file is sent |
| `FASTPROOF_TM_MAX_IMPORT_ROWS`, columns (500), JSON nesting (64), a file that cannot be read | the job ends `failed` with one report error at row 0: `limit_reached` (`maxImportRows`, `maxColumns`, `maxJsonDepth`, `maxImportBytes`) or `invalid_file` |
| `FASTPROOF_TM_MAX_IMPORT_CELL_BYTES`, an unreadable row | a report error of that row, `invalid_row`; the other rows are imported |
| steps, step depth, tags, folder depth, `FASTPROOF_TM_MAX_CASES` | a report error of the case, `limit_reached` |
| Markdown of one field over `FASTPROOF_TM_MAX_MARKDOWN_BYTES` | a report error of the case, `invalid_markdown` |
| attachment zip over its uncompressed budget, unsafe entry paths | the apply ends `failed` (`invalid_file`); nothing of that batch is applied |
| another import of the project running | `409 import_busy` |
| streamed export over 50 000 cases | `409 limit_reached` (`maxExportCases`) |

Fixed limits that are not settings:

| Limit | Value |
|---|---|
| Imports running at the same time | 1 per project, 4 per installation |
| Columns of an imported CSV file | 500 |
| Nesting of an imported JSON file | 64 levels |
| TestOps API import | 16 MiB per response, 30 s per request, 10 000 pages |
| Streamed export (`GET .../cases/export`) | 50 000 cases |
| Markdown preview (`POST .../markdown/preview`) | 64 KiB |
| Links of a case | 100, URLs up to 2048 bytes |
| Identities listed with one case | 100, the most recently seen (`identitiesTotal` counts all) |
| Folders in one `GET .../case-folders?all=true` | `FASTPROOF_TM_MAX_FOLDERS` (`truncated: true` beyond) |
| Trusted branch patterns of code sync | 50, each up to 200 bytes |
| Values in one case filter (plans, bulk changes) | 1000 |
| Step path of a manual step result | 64 bytes (`1.2.3`) |

Bulk changes, imports, exports, manual run reports, the whole folder tree and the Markdown preview count as heavy requests
(`FASTPROOF_HEAVY_QUERIES_PER_CALLER`, see [Request guards](configuration.md#request-guards)).

## Other protections

- A request that makes a handler fail unexpectedly gets the uniform 500 error body; the connection stays usable.
- Identical attachment bytes are stored once per project; each upload keeps its own content type and file name.
- Deleting a project revokes the API tokens scoped to it. Its key cannot be reused while the access file still
  grants roles in it. Its rows in ClickHouse and OpenSearch are deleted; deleted launches drop out of search at once.


- XML is parsed without DTDs and without external entities.
- Archives with entries that have absolute paths, `..` segments or NUL bytes, or that are symbolic or hard
  links, are rejected.
- Only the formats listed in [Report formats](../guide/formats.md) are parsed; other files are ignored and
  counted.
- Uploaded content is data: it is never executed on the server, HTML descriptions are sanitized, and
  attachments are served with a sandboxing Content Security Policy (see [Security](security.md)).

## Sizing hints

- The spool directory (`FASTPROOF_SPOOL_DIR`) holds uploads while they are received and processed. Plan for
  `max_upload_bytes` times the number of concurrent uploads per replica; the Helm chart sets a 4 GiB
  `emptyDir` by default.
- Large Playwright traces and videos dominate attachment storage. Retention per project removes launches
  together with the objects no other launch references.
