# Importing and exporting test cases

Bring an existing test case repository into FastProof from TestRail, Qase or Allure TestOps, or from any
spreadsheet, and take it out again whenever you want. Imports and exports are in the UI (**Test cases →
Import**, **Export**), in the API and in the CLI.

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
`qase: PROJ-45`, `testops: 12345`, or the key column of a generic CSV), so importing the same file again
finds the cases it created before:

| `mode` | A case whose key exists already |
|---|---|
| `createOnly` | is skipped |
| `upsert` | is updated, with a new [version](test-management.md#versions) (`source = import`) |

Other options:

| Option | Meaning |
|---|---|
| `format` | `generic-csv`, `testrail-csv`, `qase-csv`, `qase-json`, `testops-csv` or `fastproof-ndjson` |
| `mapping` | Generic CSV: which column fills which case field (see [Generic CSV](#generic-csv)) |
| `stepsLayout` | `rows` (one row per step, a new case when the key changes) or `cells` (all steps of a case in one cell) |
| `targetFolderId` | Folder under which the source's sections or suites are created; the top level when absent |
| `unknownFieldValues` | A value that is not an option of a select field: `create` adds the option, `skip` drops the value, `fail` reports the row as an error |

Formatting from the sources is converted to Markdown: bold, italic, code, code blocks, lists, links, images,
tables, line breaks and paragraphs; any other HTML becomes plain text. Shared steps of a source are copied into
each case as ordinary steps with the tag `imported-shared-step`. Parameters of a source are added to the
description as text.

One project runs one import at a time (`409 import_busy` otherwise), and an installation at most four.

<!-- TODO(T3): the upload request shape (multipart field names for file, options and the attachments zip), the
answer of preview and the polling and apply calls as implemented; replace the sketch below. -->

```sh
curl -fsS -X POST "$api/imports/preview" -H "$auth" -F file=@cases.csv
curl -fsS -X POST "$api/imports" -H "$auth" -F file=@cases.csv \
  -F 'options={"format":"testrail-csv","stepsLayout":"rows","mode":"upsert","unknownFieldValues":"create"};type=application/json'
curl -fsS "$api/imports/$job" -H "$auth"
curl -fsS -X POST "$api/imports/$job/apply" -H "$auth"
```

| Call | Does |
|---|---|
| `POST $api/imports/preview` | Format, headers, the first rows, suggested mapping and steps layout |
| `POST $api/imports` | Starts the dry run; `202` with the job |
| `GET $api/imports/{id}` | Status (`dry_run`, `ready`, `running`, `done`, `failed`, `cancelled`), progress and the report |
| `POST $api/imports/{id}/apply` | Applies a `ready` dry run |
| `DELETE $api/imports/{id}` | Cancels |

The same from the command line, as a dry run unless `--apply` is given:

```sh
fastproof cases import cases.csv --format testrail-csv
fastproof cases import cases.csv --format testrail-csv --apply
```

<!-- TODO(T3): CLI options as implemented (mapping, mode, target folder). -->

### Attachments

CSV exports of TestRail, Qase and TestOps do not contain attachments. Put the files in a zip next to the CSV;
FastProof matches them by file name. The zip is read with the same protections as report uploads (no absolute
paths, `..` segments or links; one byte budget for all entries). Imports through the TestOps API fetch
attachments from the source.

<!-- TODO(T3): how a CSV refers to an attachment file name per format, and the request field of the zip. -->

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

<!-- TODO(T3): column → field table (ID, Title, Section, Section Hierarchy, Priority, Type, References,
Estimate, Preconditions, Steps, Expected Result, custom fields), the separated-steps headers, what is not
imported, and which details are unconfirmed without a real export. -->

### Qase CSV and JSON

Export a project in Qase as CSV (format V1 or V2, detected from the first column) or JSON. Suites become
folders; the case id (`PROJ-45`) becomes the key. Qase statuses map to case statuses: `actual` → `active`,
`draft` → `draft`, `deprecated` → `outdated`.

<!-- TODO(T3): CSV column → field table (steps_actions/steps_results/steps_data, priority, severity, type,
behavior, automation, layer, tags, custom fields), suite rows, Gherkin steps, JSON fields used, unconfirmed
details. -->

### Allure TestOps CSV

Export test cases from TestOps as CSV. The Allure ID becomes the key `testops`. When the project in FastProof is
empty, imported cases also keep their Allure ID as their **case number**, so `@AllureId("45123")` in the code
keeps working without changes and new cases are numbered after the highest imported one. In a project that
already has cases, the Allure ID is only a key.

<!-- TODO(T3): columns read (allure_id, name, description, precondition, scenario, expected_result, tags,
status, layer, links), the step and sub-step encoding, unconfirmed details. -->

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

<!-- TODO(T3): the flag for http, which TestOps versions were tested, what is imported per entity, how
workflows and statuses map. -->

### Generic CSV

Any CSV with a header row: map its columns to case fields in the preview, or pass `mapping` (column name →
field). One column should hold a stable id from the source, so a second import updates instead of
duplicating.

<!-- TODO(T3): the list of mapping targets (name, folder path and its separator, description, preconditions,
step action / expected / data, status, tags and their separator, fields.<key>, key), how steps in rows and in
cells are recognised, an example file. -->

### FastProof NDJSON

The format of the FastProof [export](#exporting): one JSON object per case with everything a case has
(folders, fields, tags, steps, links, keys). Use it to move cases between projects or servers without losing
anything.

<!-- TODO(T3): the object shape or a link to its schema, and what happens to attachments and identities. -->

## Exporting

| Call | Gives |
|---|---|
| `GET $api/cases/export?format=csv` | CSV, streamed, one row per case |
| `GET $api/cases/export?format=ndjson` | NDJSON, streamed, every detail of every case |
| `POST $api/cases/exports` `{format: "zip", filter, attachments: true}` | A background job that builds a zip with the cases and their attachments |

Exports take the same filters as the case list (`folder`, `recursive`, `status`, `field`, `tag`, `q`, ...)
and hold at most 50 000 cases; they need only `cases:read`.

```sh
curl -fsS "$api/cases/export?format=ndjson&folder=$folder&recursive=true" -H "$auth" -o cases.ndjson
fastproof cases export --format csv --out cases.csv
```

- **CSV** cells that start with `=`, `+`, `-`, `@`, a tab or a carriage return are prefixed with `'`, so a
  spreadsheet never runs them as formulas.
- **Zip** exports are stored for 24 hours behind a signed link that works once and only for the person who
  created the export.

<!-- TODO(T3): CSV columns, the zip layout, how the zip job is polled and downloaded. -->

## Permissions

| Permission | Allows | Roles |
|---|---|---|
| `cases:import` | Preview, run, apply and cancel imports | `editor` |
| `cases:read` | Export | `viewer`, `tester`, `uploader`, `editor` |

Imports and exports are written to the audit log (`import.start`, `import.apply`, `import.cancel`,
`export.create`).
