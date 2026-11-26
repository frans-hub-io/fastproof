# Parameters

A **data-driven** test case runs the same steps with different data: a checkout in several browsers and
currencies, a login with several roles. Instead of copying the case, give it a **parameter table**: columns
(`browser`, `currency`) and rows of values. Each run executes the case **once per row**, as one item in the
queue with an **iteration** per row, and every iteration gets its own result.

The examples on this page use:

```sh
api=https://fastproof.example.com/api/v1/projects/web
auth="Authorization: Bearer $FASTPROOF_TOKEN"
```

## Placeholders

Write `${name}` where a value goes, in the preconditions or in the action, expected result or test data of a
step:

```text
Preconditions: Open the shop in ${browser}
Step 2:        Pay in ${currency}          Expected: The receipt shows ${currency}
```

- A placeholder is `${` + a column name + `}`. Column names start with a letter or `_` and go on with letters,
  digits, `_`, `.` or `-` (at most 64 characters).
- In a case **with** parameters every placeholder must name a column. An unknown one is refused with 400
  `invalid_case`, and `details` names the field, such as `body.steps[1].expected`. The steps editor marks
  placeholders: blue for a column of the case, red for an unknown one.
- In a case **without** parameters, `${…}` is plain text (a shell variable in a step stays as it is).
- Placeholders inside the steps of a [shared step](shared-steps.md) take the values of the case that runs it.
  There an unknown name stays as it is.
- Values are plain text. In the runner they replace the placeholder in the text of the step; they never become
  links or formatting.

## The parameter table

Open the **Parameters** tab of a case and choose **Add parameters** (or **Edit parameters**). The table has a
column per parameter and a row per data set:

- **Add column**, **Add row**, or **Paste rows** from a spreadsheet (one row per line, cells separated by tabs;
  the first line can name the columns).
- Every row has a **Run** switch. A switched-off row stays in the table but does not run.
- **Generate…** fills the table from a list of values per parameter (one value per line):
  - **All combinations**: one row for every combination.
  - **Pairwise**: every pair of values of two parameters appears in at least one row. Three parameters with
    three values each need 27 rows for all combinations and about 9 pairwise. The generator is deterministic:
    the same input always gives the same rows.

  Generated rows replace the table; nothing is saved until you save the case. The case keeps the generator
  input, so **Generate…** opens with it next time.

Saving the parameters saves a new **version** of the case, like any other change to its content. With
[review](workflows-and-review.md) on, a change of the parameters needs approval like a change of the steps.

In the API the table is `body.parameters` of the case:

```json
{"columns": ["browser", "currency"],
 "rows": [{"id": "r_01JD6Z2Q8M4X7V3N5K9T2R6W1Y", "values": {"browser": "firefox", "currency": "EUR"}, "enabled": true},
          {"id": "r_01JD6Z2Q8M4X7V3N5K9T2R6W1Z", "values": {"browser": "chromium", "currency": "USD"}, "enabled": true}],
 "generator": {"mode": "pairwise", "values": {"browser": ["firefox", "chromium"], "currency": ["EUR", "USD"]}}}
```

```sh
curl -s -X POST "$api/cases/42/parameters/generate" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"mode": "pairwise", "values": {"browser": ["firefox", "chromium"], "currency": ["EUR", "USD", "UAH"]}}'
```

`POST /cases/{n}/parameters/generate` (`cases:write`) answers `{"rows": [...]}` with new row ids and saves
nothing; put the rows into `body.parameters.rows` and save the case with `PUT /cases/{n}` and `If-Match`.

Rules the server checks on every save:

| Rule | Limit | Error |
|---|---|---|
| Columns | 20 (`FASTPROOF_TM_MAX_PARAMETER_COLUMNS`), names unique | 409 `limit_reached`, 400 `invalid_case` |
| Rows | 1000 (`FASTPROOF_TM_MAX_PARAMETER_ROWS`) | 409 `limit_reached` |
| A value | 1024 bytes (`FASTPROOF_TM_MAX_PARAMETER_VALUE_BYTES`), no control characters except tab and line break | 400 `invalid_case` |
| Row ids | `r_` and up to 62 letters, digits, `_` or `-`, unique | 400 `invalid_case` |
| Values | only for columns of the table; a missing value is empty | 400 `invalid_case` |
| Enabled rows | no two with the same values, compared as Allure parameters (`name:value,…`, so values with `,` or `:` that spell another row count as the same) | 400 `invalid_case` |
| A text with values | at most the Markdown limit of a field with the longest value of each column put in | 400 `invalid_case` |
| Generator input | 20 parameters, 500 values in all | 409 `limit_reached` |

A case that does not have parameters yet gets them only once every server of the installation runs a version
that knows them; during a rolling upgrade the save answers 409 `upgrade_in_progress` (see
[upgrading](../operations/upgrading.md)).

## Running a parameterized case

When a parameterized case is added to a manual run, its run item gets one **iteration per enabled row** of the
case version the run executes. The rows and values are fixed at that moment; editing the table later changes
the next runs only. A launch holds at most 50 000 iterations (`FASTPROOF_TM_MAX_RUN_ITERATIONS`); adding cases
beyond that is refused with 409 `limit_reached` and adds nothing. The same happens when the parameter tables
of the cases being added come to more than 64 MiB (`maxIterationBytes`).

In the runner the case shows a strip of iterations (`#1`, `#2`, ...) with their status. **[** and **]** move
between them. The current iteration lists its values, and the preconditions and steps with the values put in.
Record the steps and finish the iteration as you would a case; the runner then moves to the next open
iteration. **Apply to all…** sets every iteration to one status, after a confirmation.

The status of the item follows its iterations:

| Iterations | Item |
|---|---|
| any failed | failed |
| else any blocked | blocked |
| all untested | untested |
| all skipped | skipped |
| some open and some started or finished | in progress |
| otherwise (all finished, passed or skipped) | passed |

Closing the manual run with "untested to skipped" skips every open iteration, also those of an item that is
already failed or blocked through another iteration. The item keeps its derived status.

A long item stays quick to open: `GET /run-items/{itemId}` carries the step results of the first iterations up
to a size budget (256 KiB of step texts). Later iterations have `stepResultsOmitted: true` and an empty
`stepResults`; the runner reads the step results of the iteration you open on its own
(`GET /run-items/{itemId}/iterations/{rowId}`).

## Defects

A defect belongs to a result, and each iteration has its own. In the runner:

- **Defect** on an iteration (key **d**, once the iteration is finished) opens the defect drawer for that
  iteration. The comment starts with the row, its values and the first failed or blocked step with the values put
  in. The defect and the new-issue link are for that iteration only.
- **Defect…** above the strip is for the item as a whole. It puts the defect on every failed or blocked
  iteration (at most 1000), and the comment lists them (the first 10, then a count). The item shows the defect of
  its first iteration that has one.

Both need a finished iteration to put the defect on, as for a plain case; otherwise the answer is 409
`run_item_not_final`. A holder of `runs:execute` can set it on iterations they executed.

## Reports and earlier runs

The manual run report lists the iterations of a parameterized case, in the printable page and in the export:

- the printable page (`/launches/{id}/manual/print`) shows, per iteration, its values, status, comment, defect
  and the recorded steps with the values put in;
- the JSON report has `parameterColumns` and `iterations` per item (`steps` stays empty: steps belong to the
  iterations). The step results of one item are cut at 8 MiB, then `stepsOmitted` is set on the rest;
- the CSV report has a last column `iteration` (`#2 browser=chromium · currency=EUR`) and one row per iteration
  after its item, with that iteration's status, executor, times, comment, defect and steps.

When a case is run again, the iterations of the earlier run stay: the **Earlier runs** card of the case lists them
with values, status and step results (the failed ones open). A page of earlier runs carries at most 100 iterations
per run, within 1 MiB; `iterationCount` says how many there were.

## Results per data set

Each finished iteration is its own result in the launch, on a series of the case with the row's values as
**parameters**. That is the same series key an automated test gets from Allure `@Parameter`s with the same names
and values (`parameters_hash`). So:

- the case's history groups results per data set, manual and automated side by side;
- the result page shows the values under **Parameters**, and the steps with the values put in;
- quality gates and dashboards count every data set, and a failed iteration fails the launch like a failed
  automated test (`blocked` is stored as `broken`).

The run item itself has no result of its own.

## API

| Method and path | Permission | What it does |
|---|---|---|
| `POST /cases/{n}/parameters/generate` | `cases:write` | rows from a generator input, nothing saved |
| `GET /run-items/{itemId}` | `launches:read` | `iterations` (row id, position, values, status, comment, result, step results) and `parameterColumns`; `iterationCount` in lists |
| `GET /run-items/{itemId}/iterations/{rowId}` | `launches:read` | one iteration with all its step results; `?executionNo=` for an earlier run |
| `PUT /run-items/{itemId}/iterations/{rowId}/steps/{path}` | `runs:execute` | one step result of an iteration; no `If-Match` (the last writer wins) |
| `PATCH /run-items/{itemId}/iterations/{rowId}` | `runs:execute` | `status`, `comment`, `untestedStepsTo`; needs the item's `If-Match` |
| `PATCH /run-items/{itemId}` with `allIterations: true` | `runs:execute` | one status for every iteration |

`POST /run-items/{itemId}/defect` takes `rowId` for one iteration (without it, every failed or blocked iteration),
and `GET /run-items/{itemId}/new-issue-url` takes `rowId` too: the title and text name the iteration, its values
and its failing step.

On a parameterized item, `PUT /run-items/{itemId}/steps/{path}` and a `PATCH /run-items/{itemId}` with a status
but without `allIterations` answer 409 `parameterized_item`: the steps and status belong to the iterations. The
item's comment and assignee change as usual.

```sh
curl -s -X PUT "$api/run-items/$item/iterations/r_01JD6Z2Q8M4X7V3N5K9T2R6W1Y/steps/2" -H "$auth" \
  -H 'Content-Type: application/json' -d '{"status": "failed", "actual": "The receipt shows USD"}'
```

Every iteration change emits `runIteration.updated {launchId, itemId, rowId, status, revision}` and
`runItem.updated` on the live stream, and a status change is in the audit log as `runIteration.update`.
