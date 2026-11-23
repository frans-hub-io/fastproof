# Importing and exporting test cases

Bring an existing test case repository into FastProof from TestRail, Qase or Allure TestOps, or from any
spreadsheet, and take it out again whenever you want. Imports and exports are in the UI (**Test cases →
Import**, **Export**), in the API and in the CLI.

!!! note "How the formats were checked"
    The TestRail, Qase and Allure TestOps formats were built and tested against the columns and structures the
    vendors document (and, for TestOps, its official migration script), not against real exports from those
    tools. Each source below lists what could not be confirmed. Run a dry run first and check its report before
    you apply an import.

The examples on this page use:

```sh
api=https://fastproof.example.com/api/v1/projects/web
auth="Authorization: Bearer $FASTPROOF_TOKEN"
```

## How an import works

Every import goes through the same three stages, and nothing changes in the project until the last one:

1. **Preview.** Upload the file; FastProof reads its first 5 MB, detects the format, shows the first rows and
   proposes a mapping of columns to case fields and a steps layout.
2. **Dry run.** Start the import with the options you chose. The server reads the whole file and answers with
   a report: how many cases it would create, update and skip, which folders it would create, unknown fields and
   field values, and errors per row (the first 1000).
3. **Apply** the dry run, or cancel it.

Imports are **idempotent**. Each imported case remembers its key in the source (`testrail: C123`,
`qase: 45` from a CSV or `qase: PROJ-45` from a JSON file that carries the project code, `testops: 12345`,
or the key column of a generic CSV), so importing the same file again finds the cases it created before:

| `mode` | A case whose key exists already |
|---|---|
| `createOnly` (the default) | is skipped |
| `upsert` | is updated, with a new [version](test-management.md#versions) (`source = import`); a case whose content did not change is skipped and gets no version |

Other options:

| Option | Meaning |
|---|---|
| `format` | `generic-csv`, `testrail-csv`, `qase-csv`, `qase-json`, `testops-csv` or `fastproof-ndjson` |
| `mapping` | Generic CSV: which column fills which case field (see [Generic CSV](#generic-csv)) |
| `stepsLayout` | `rows` (one row per step, a new case when the key changes) or `cells` (all steps of a case in one cell) |
| `targetFolderId` | Folder under which the source's sections or suites are created; the top level when absent |
| `unknownFieldValues` | Default `skip`. A value that is not an option of a select field: `create` adds the option, `skip` drops the value, `fail` reports the row as an error. With `create`, source columns that match no case field are also created as fields (see [Unknown columns](#unknown-columns-and-custom-fields)) |
| `status` | Optional, `draft` or `active`: every case the import creates gets it. Absent: each created case gets the status its source maps (TestOps, Qase and NDJSON statuses), else `active`. Updated cases keep the source's status, or their own. In the UI: **Status of new cases** |

Formatting from the sources is converted to Markdown: bold, italic, code, code blocks, lists, links, images,
tables, line breaks and paragraphs; any other HTML becomes plain text. Shared steps of a source are copied into
each case as ordinary steps with the tag `imported-shared-step`. Parameters of a source are added to the
description as text.

One project runs one import at a time (`409 import_busy` otherwise), and an installation at most four.

### Unknown columns and custom fields

Columns of a source that match no case field (by key or name) are listed in the dry run report under
`unknownFields`. With `unknownFieldValues: skip` or `fail` their values are left out. With
`unknownFieldValues: create` (in the UI: **Create fields and options**), the import creates a custom field for
each such column, at most 50 per import and up to the project's field limit. The field is named after the
column (a parser key such as `automation_type` becomes "Automation type"), its key is derived from the name
(`_2` on a collision, an `f_` prefix when it would not start with a letter), and its type follows the values:
`select` with the seen values as options when the column has 1 to 50 distinct values of at most 200 bytes,
`multiselect` when cells hold lists, `number` when every value is a number, otherwise `text`. So a default
TestRail import keeps `Type`, `References`, `Estimate`, `Milestone`, `Automation Type` and the custom columns
(`Component` maps to the built-in `component`). The report lists these fields under `createdFields` (a dry
run lists the ones its apply will create), not under `unknownFields`; the apply creates them before the first
batch. To control the type or name of a field, create it on the [Case fields](test-management.md#custom-fields)
page before the import, or redirect the column to another field in the mapping.

### Required fields and the status of imported cases

A [required](test-management.md#custom-fields) custom field does not fail imported rows. An imported case
that has no value for a required field (a default counts for new cases) is imported as `draft`, whatever
status the source or the `status` option gives it, and the report lists it under `warnings` with the code
`required_fields_missing` ("imported as draft: no value for …"). This also applies when an upsert updates a
case, so a field made required later can turn an active case back into a draft. Fill in the value and
activate the case later, for example with a bulk change.

A file import is a `multipart/form-data` request with the fields `options` (the options above as JSON),
`file` (the CSV, JSON or NDJSON file) and, optionally, `attachments` (a zip). Other fields are refused.

```sh
curl -fsS -X POST "$api/imports/preview" -H "$auth" -F file=@cases.csv
job=$(curl -fsS -X POST "$api/imports" -H "$auth" \
  -F 'options={"format":"testrail-csv","stepsLayout":"rows","mode":"upsert","unknownFieldValues":"create"}' \
  -F file=@cases.csv -F attachments=@files.zip | jq -r .id)
curl -fsS "$api/imports/$job" -H "$auth"          # poll until "status": "ready"
curl -fsS -X POST "$api/imports/$job/apply" -H "$auth"
curl -fsS "$api/imports/$job" -H "$auth"          # poll until "status": "done"
```

The preview answers `{format, headers, rows, suggestedMapping, stepsLayout}`: the detected format (or the one
given as `?format=`), the header, the first 20 rows, the proposed mapping and the detected steps layout. For a
generic CSV the mapping covers every column; for the tool formats, which know their own columns, it lists only
the columns that become custom fields (`field:<column>`), which you may redirect to another field or set to
`ignore`.

| Call | Does |
|---|---|
| `POST $api/imports/preview` | Format, headers, the first rows, suggested mapping and steps layout (nothing is stored) |
| `POST $api/imports` | Starts the dry run; `202` with the job (`TmJob`) |
| `GET $api/imports/{id}` | Status (`queued`, `dry_run`, `ready`, `running`, `done`, `failed`, `cancelled`), progress `{done, total}` and the report |
| `POST $api/imports/{id}/apply` | Applies a `ready` dry run; `409 job_not_ready` otherwise |
| `DELETE $api/imports/{id}` | Cancels an import that is not finished (`409 job_finished` otherwise); batches applied already stay |
| `GET $api/tm-jobs/{id}` | Any import, export or backfill job of the project (`cases:read`; an import job needs `cases:import`, an export only for its creator) |

The report has `create`, `update`, `skip` (matches skipped by `createOnly` and unchanged cases), `folders`,
`createdFields` (fields the import creates), `warnings` (rows imported with a change, `{row, code, message}`), `unknownFields` (source fields without a case field whose values are
left out, see [above](#unknown-columns-and-custom-fields)), `unknownValues` (per field, the
values that are not options) and `errors` (`{row, code, message}`, the first 1000; `row` is the CSV row with the
header as row 1, or the line of an NDJSON file). Row codes: `invalid_row` (unreadable row, cell over the limit),
`invalid_case` (no name, invalid content), `invalid_markdown`, `limit_reached` (steps, depth, tags, folders,
cases), `unknown_value` (with `unknownFieldValues: fail`). An import that cannot be read at all ends `failed`
with one error at row 0 (`invalid_file`, `limit_reached`). Apply works in batches of 500 cases, each committed
with the progress, so an interrupted import continues where it stopped.

The same from the command line, as a dry run unless `--apply` is given:

```sh
fastproof cases import cases.csv --format testrail-csv
fastproof cases import cases.csv --format testrail-csv --apply
```

| Option | Default | Meaning |
|---|---|---|
| `--format <f>` | required | The format, as `format` above |
| `--mode createOnly\|upsert` | `createOnly` | |
| `--unknown-values skip\|create\|fail` | `skip` | `unknownFieldValues` |
| `--steps-layout rows\|cells` | detected | |
| `--mapping map.json` | suggested | A JSON object `{column: target}` (generic CSV) |
| `--attachments files.zip` | none | The zip of attachment files |
| `--folder <id>` | top level | `targetFolderId` |
| `--apply` | dry run only | Apply after the dry run |

The command prints the report to stderr and exits `0`, or `2` when the import failed. It needs `FASTPROOF_URL`,
`FASTPROOF_TOKEN` (or `--token-file`) and `--project` (`FASTPROOF_PROJECT`), as the other commands.

### Attachments

CSV exports of TestRail, Qase and TestOps do not contain attachments. Put the files in a zip next to the CSV;
FastProof matches them by file name. The zip is read with the same protections as report uploads (no absolute
paths, `..` segments or links; one byte budget for all entries). Imports through the TestOps API fetch
attachments from the source.

Send the zip in the `attachments` field of the import request (`--attachments` in the CLI). A zip entry
matches by its path in the zip or by its file name alone. Cases refer to files:

- in a generic CSV, through a column mapped to `attachments` (file names separated by commas or new lines);
- in Markdown of any text (description, steps), as an image or link whose target is the file name:
  `![screen](login.png)`; such references become `attachment:<id>` and show the image;
- in a Qase JSON export, through the `attachments` of a case or step (`filename`);
- in a FastProof NDJSON export, through the attachment names it lists (the `attachments/` entries of a
  [zip export](#exporting)).

Each file is at most `FASTPROOF_TM_MAX_ATTACHMENT_BYTES` (50 MiB); HTML and SVG files are stored as
`application/octet-stream`.

### Limits

| Limit | Default |
|---|---|
| CSV or JSON file | 100 MiB |
| Zip of attachments, uncompressed | 2 GiB |
| Rows | 200 000 |
| One cell | 64 KiB |
| Columns | 500 |
| JSON nesting | 64 levels |

CSV files may be UTF-8 or UTF-16 with a byte order mark. Formulas in cells are read as text, never evaluated.
See [Limits](../operations/limits.md#test-management) for the settings.

## Sources

### TestRail CSV

Export the cases in TestRail with **Export → CSV** and include every column. Both step layouts work: steps in
one cell (`Steps (Expected Result)`) and "separated steps on separate rows". Sections become folders, nested by
`Section Hierarchy` (levels separated by `>`); the case id (`C123`) becomes the key. HTML from TestRail's
WYSIWYG editor is converted to Markdown.

| Column | Becomes |
|---|---|
| `ID` (`C123`) | key `testrail` |
| `Title` | name |
| `Section Hierarchy` (`A > B > C`), else `Section` | folder path |
| `Preconditions` | preconditions |
| `Steps (Step)`, `Steps (Expected Result)`, `Steps (Additional Info)` | steps: action, expected result, data |
| `Steps` and `Expected Result` (template "Test Case (Text)") | one step |
| `Mission`, `Goals` (template "Exploratory Session") | the description |
| `Steps (Shared step ID)` | the tag `imported-shared-step` |
| `Priority` | the built-in field `priority` |
| `Type`, `References`, `Estimate`, `Milestone`, `Automation Type` | fields `type`, `references`, `estimate`, `milestone`, `automation_type`; they are not built in, so they are kept when the fields exist or when `unknownFieldValues: create` creates them (see [Unknown columns](#unknown-columns-and-custom-fields)) |
| any other column | a field of that name (matched to a case field by key or name), created the same way |

Not imported: `Section Depth`, `Section Description`, `Suite`, `Suite ID`, `Template`, `Created/Updated By/On`,
`is_converted`. With separated steps, a row whose `ID` is empty or repeats the case above adds a step to it; with
steps in one cell, the cells hold numbered lists (`1. …`, sub-steps indented with a tab or numbered `1.2.`) and
the expected results are matched to the steps by position. The layout is detected when it is not given.

With separated steps, every step is one row of the file, and the import limit counts rows, not cases: at the
default of 200 000 rows (`FASTPROOF_TM_MAX_IMPORT_ROWS`), 20 000 cases of 12 steps do not fit, and the import
ends `failed` with `limit_reached`. Raise the limit (see [Limits](../operations/limits.md)) or export the steps
in one cell per case.

Not confirmed on a real export (TestRail's documentation was the source): the exact header of the one-cell
layout and its numbering, whether separated-step rows repeat the `ID` (both forms are read), and the spacing
around `>` in `Section Hierarchy`.

### Qase CSV and JSON

Export a project in Qase as CSV (format V1 or V2, detected from the first column) or JSON. Suites become
folders; the case id (`PROJ-45`) becomes the key. Qase statuses map to case statuses: `actual` → `active`,
`draft` → `draft`, `deprecated` → `outdated`.

| CSV column | Becomes |
|---|---|
| `id` (V1) or `v2.id` (V2) | key `qase` |
| `title` | name |
| `description`, `preconditions` | description, preconditions |
| `postconditions`, `parameters` | sections added to the description |
| `steps_actions`, `steps_result` (or `steps_results`), `steps_data` | steps: numbered lists (V2: each step in double quotes); sub-steps indented with a tab |
| `priority`, `severity`, `type`, `behavior`, `automation`, `layer`, `is_flaky`, `milestone` | fields of these names (`undefined` and `not set` are no value) |
| `status` | `draft`, `actual` → `active`, `deprecated` → `outdated` |
| `tags` | tags, separated by commas |
| any other column | a custom field of that name (Qase names these columns by field id) |

Suites are rows with `suite_without_cases = 1`, a `suite_id`, a `suite_parent_id` and the title in `suite` (or
`title`); they come before the cases. A case finds its suite through `suite_id`, else `suite_parent_id`, else the
`suite` title. Gherkin steps (`steps_type = gherkin`) are imported line by line as steps.

Qase JSON is read in the shape of the Qase API v1: `{"suites": [{"title", "cases", "suites"}]}`,
`{"result": {"entities": [...]}}`, `{"cases": [...]}` or an array of cases, each with `id`, `title`,
`description`, `preconditions`, `postconditions`, `severity`, `priority`, `type`, `layer`, `status` (a slug or
0/1/2), `tags`, `custom_fields` (`{title|name, value}`), `steps` (`{action, expected_result, data, attachments,
steps, shared_step_hash}`) and `attachments`. A root `code` (or `project_code`) makes keys `CODE-45`.
The file is read as a stream, one case at a time, so its size does not decide the memory an import needs; one
case may hold at most 8 MiB of JSON (larger ones become a `limit_reached` row error). For that, a root `code`
must come before the cases it applies to and a flat suite list (`suites_flat`, `suiteList`) before every case,
as in Qase's own exports; a file that has them later fails with `invalid_file`. Entries of flat suite lists count
toward the row limit together with the cases. A suite's `title` should come before its `cases`: cases that wait
for a later title may hold at most 32 MiB in all, beyond which the file fails with `invalid_file`. Suite names
longer than the name limit are cut while reading (such cases are then reported as having a too long folder
name), and a suite path follows parents at most one level past the folder depth limit and stops at a cycle.
A JSON string counts toward the cell limit with its decoded length (`\u` escapes as 3 bytes).

Not confirmed on real exports: the V2 step encoding beyond the quotes and tabs, the column with a suite's title,
whether case rows name their suite in `suite_id` or `suite_parent_id`, and the JSON export schema (Qase does not
publish it).

### Allure TestOps CSV

Export test cases from TestOps as CSV. The Allure ID becomes the key `testops`. When the project in FastProof is
empty, imported cases also keep their Allure ID as their **case number**, so `@AllureId("45123")` in the code
keeps working without changes and new cases are numbered after the highest imported one. In a project that
already has cases, the Allure ID is only a key.

TestOps does not publish its CSV columns; FastProof reads the generic CSV of the official migration script
`tom`, and matches headers without case, spaces and punctuation (`Allure ID`, `Expected result` work too):

| Column | Becomes |
|---|---|
| `allure_id` | key `testops` and, in an empty project, the case number |
| `name`, `description`, `precondition` | name, description, preconditions |
| `scenario` | steps: one line per step, a tab per nesting level (a line with one tab is a sub-step) |
| `expected_result` | the expected result of the last step (the description when there are no steps) |
| `tags` | tags, separated by commas |
| `status` | `Draft`, `Active`, `Outdated` (and similar words) → the case status |
| `layer`, `tested_feature`, `tested_story` | fields `layer`, `feature`, `story` |
| `links` / `useful_links`, `issue_tracker` | links and issue links (`http`/`https` URLs) |
| `full_name` with `automated = true` | a code identity linked to the case |
| any other column | a field of that name |

Not confirmed on a real TestOps export: its own column names.

### Allure TestOps API (beta)

Instead of a file, FastProof can read the cases directly from a TestOps instance with an API token:
scenarios with nested steps, custom field values, links and attachments. Automated TestOps cases with a known
full name are linked to their test at once, so the first upload of the test does not create a second case.

```sh
curl -fsS -X POST "$api/imports" -H "$auth" -H 'Content-Type: application/json' -d '{
  "source": "allure-testops",
  "baseUrl": "https://testops.example.com",
  "token": "<TestOps API token>",
  "remoteProjectId": 7,
  "mode": "createOnly"
}'
```

- The TestOps host must be on the server's egress allowlist (`FASTPROOF_EGRESS_ALLOW`, see
  [Configuration](../operations/configuration.md#integrations)); only `https` URLs are accepted, and
  redirects to other hosts are refused.
- The token is stored encrypted as a project secret at once and never returned, logged or written to the
  audit log.
- Each response is limited to 16 MiB, each request to 30 s, an import to 10 000 pages.

- Set `FASTPROOF_TESTOPS_ALLOW_HTTP=true` to allow an `http` base URL (a test instance on a closed network);
  the default is `https` only.
- The import needs `FASTPROOF_SECRETS_KEY_FILE` (`503` otherwise). The token is bound to the origin of the base
  URL, read only by the import job and deleted when the import is done or cancelled.
- The dry run reads every case of the project (search pages of 100, then for each case the case, its scenario,
  custom field values, links, issues and the attachment list) and keeps them for the apply, which downloads
  the attachments (each at most the attachment limit, together at most the zip budget).
- What is imported: name, description, precondition, the scenario with nested steps, expected results and
  step attachments, the expected result of the case, status (by its name), layer, tags, custom field values
  (fields of the same name), links and issues, attachments. Workflows are not imported; folders are not read
  from TestOps (its trees are separate objects), so the cases land in the target folder.
- Paths start with `/api/rs`; when the search answers 404, FastProof uses `/api` (newer versions).

Not tested against a real TestOps instance yet: the API paths, the search query (`id > 0`), the scenario
format of each version, and the `/api/rs` and `/api` variants were taken from the official utilities and
checked only against a test double.

### Generic CSV

Any CSV with a header row: map its columns to case fields in the preview, or pass `mapping` (column name →
field). One column should hold a stable id from the source, so a second import updates instead of
duplicating.

Mapping targets:

| Target | Column holds |
|---|---|
| `name` | the case name (required) |
| `key` | a stable id from the source (key `external`) |
| `folder` | the folder path: `A / B` or `A > B` |
| `description`, `preconditions` | text (HTML becomes Markdown) |
| `steps`, `expected`, `data` | step action, expected result, test data |
| `status` | `draft`, `active`, `outdated` (also `actual`, `deprecated` and similar words) |
| `tags` | tags separated by commas, semicolons or new lines |
| `links` | `http`/`https` URLs (optionally `name: url`), separated by commas or new lines |
| `attachments` | file names in the attachment zip |
| `fullName` | a code full name (kept as information) |
| `field:<key>` | a case field, by key or name |
| `ignore` | nothing |

Without `mapping`, the suggested one is used. Steps in **cells**: the step cells hold numbered lists, matched by
position; steps in **rows**: a row without a name and key adds a step to the case above, and so does a row with
the same key.

```csv
Key,Summary,Folder,Steps,Expected,Priority
EXT-1,Create an order,Orders / New,"1. Open the form
2. Save","1. The form is shown
2. The order is saved",high
```

### FastProof NDJSON

The format of the FastProof [export](#exporting): one JSON object per case with everything a case has
(folders, fields, tags, steps, links, keys). Use it to move cases between projects or servers without losing
anything.

The first line is a header: `{"format": "fastproof-cases", "version": 1, "project", "exportedAt", "fields":
[{key, name, type, options, sourceLabel}]}`; then one case per line: `{"keys", "number", "name", "folder":
[names], "status", "description", "preconditions", "steps": [{id, action, expected, data, attachments, steps}],
"links", "tags", "fields", "automated", "fullNames", "attachments": [{name, source, oldId}]}`.

- Into a project **without cases**, cases keep their numbers.
- With `unknownFieldValues: create`, field definitions of the header that the project lacks are created; the
  report lists them as created.
- Step ids are kept. Attachments come from a zip given with the import (the `attachments/` entries of a zip
  export); `attachment:<id>` images are rewritten to the new ids.
- Code identities (`fullNames`) are not recreated: the first upload of the tests links them again through code
  sync.

## Exporting

| Call | Gives |
|---|---|
| `GET $api/cases/export?format=csv` | CSV, streamed, one row per case |
| `GET $api/cases/export?format=ndjson` | NDJSON, streamed, every detail of every case |
| `POST $api/cases/exports` `{format: "zip", filter, attachments: true}` | A background job that builds a zip with the cases and their attachments |

Exports take the same filters as the case list (`folder`, `recursive`, `status`, `field`, `tag`, `q`, ...)
and hold at most 50 000 cases; they need only `cases:read`. Zip export jobs are limited: at most 2 queued or
running per caller and project and 4 per project; a new one over that answers `409 export_busy` (see
[Limits](../operations/limits.md#test-management)). Exports run in their own worker slots, so they never hold
up imports.

```sh
curl -fsS "$api/cases/export?format=ndjson&folder=$folder&recursive=true" -H "$auth" -o cases.ndjson
fastproof cases export --format csv --out cases.csv
```

- **CSV** cells that start with `=`, `+`, `-`, `@`, a tab or a carriage return are prefixed with `'`, so a
  spreadsheet never runs them as formulas.
- **Zip** exports are stored for 24 hours behind a signed link that works once and only for the person who
  created the export.

CSV columns: `Number`, `Key` (`TC-<n>`), `Name`, `Status`, `Automated`, `Folder` (`A / B`), `Description`,
`Preconditions`, `Steps`, `Expected`, `Data` (numbered lists, sub-steps indented with a tab), `Tags`, `Links`,
then one column per case field (its key). This CSV imports back as a generic CSV.

The zip contains `cases.ndjson`, `cases.csv` and `attachments/<id>-<file name>`. The answer of
`POST $api/cases/exports` carries `downloadUrl` (only there); poll `GET $api/tm-jobs/{id}` until `"status":
"done"`, then fetch the link once with the same token:

```sh
j=$(curl -fsS -X POST "$api/cases/exports" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"format":"zip","attachments":true}')
curl -fsS "$api/tm-jobs/$(echo "$j" | jq -r .id)" -H "$auth"   # until "status": "done"
curl -fsS "https://fastproof.example.com$(echo "$j" | jq -r .downloadUrl)" -H "$auth" -o cases.zip
```

## Permissions

| Permission | Allows | Roles |
|---|---|---|
| `cases:import` | Preview, run, apply and cancel imports | `editor` |
| `cases:read` | Export | `viewer`, `tester`, `uploader`, `editor` |

Imports and exports are written to the audit log (`import.start`, `import.apply`, `import.cancel`,
`export.create`, `export.download`). The audit never holds a TestOps token.
