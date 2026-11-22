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
| `max_name_bytes` | 4 KiB | Kept length of a test name and full name; longer names are truncated with a warning |
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

<!-- TODO(T3): the answer codes of the import limits (rows, cells, columns) as implemented. -->

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
| Identities listed with one case | 100, the most recently seen |
| Trusted branch patterns of code sync | 50, each up to 200 bytes |
| Values in one case filter (plans, bulk changes) | 1000 |
| Step path of a manual step result | 64 bytes (`1.2.3`) |

Bulk changes, imports, exports, manual run reports and the Markdown preview count as heavy requests
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
