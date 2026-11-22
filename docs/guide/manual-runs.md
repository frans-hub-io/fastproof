# Test plans and manual runs

A **test plan** chooses the [test cases](test-management.md) to run, for example a release regression. A
launch started from a plan runs its manual cases by hand in FastProof and its automated cases in CI, and both
parts land in the same launch, with the same history, counts, quality gates and notifications as any other
launch.

The examples on this page use:

```sh
api=https://fastproof.example.com/api/v1/projects/web
auth="Authorization: Bearer $FASTPROOF_TOKEN"
```

## Managed plans

Plans built from cases are **managed** plans; open **Test plans** in the project navigation. (The short-lived
plans that `fastproof testplan --from-launch` creates, for example "the failed tests of launch 42", are a
different kind and expire after 90 days; managed plans stay until they are archived.)

A plan holds cases in two ways, and can use both:

- **explicitly**: cases you add by number or pick in the tree (a folder adds every case below it);
- **by filter**: a saved filter with the same parameters as the case list: `folderIds`, `recursive`,
  `statuses`, `automation`, `fields` (`{"key": ["value", ...]}`), `tags` (all must match) and `q`. In a plan
  filter, `recursive` defaults to `true`: a folder also takes the cases of its subfolders ("in X and below"),
  unless the filter says `"recursive": false`. Plans store `recursive: true` explicitly, and older filters
  without the key are read the same way. (The case list itself keeps `recursive=false` as its default.)

```sh
curl -fsS -X POST "$api/test-plans" -H "$auth" -H 'Content-Type: application/json' -d '{
  "kind": "managed",
  "name": "Release 2.4 regression",
  "description": "Everything with priority high, plus the payment smoke cases.",
  "caseNumbers": [12, 13, 14],
  "filter": {"statuses": ["active"], "fields": {"priority": ["high", "critical"]}}
}'
```

The filter is applied when the plan is created. A plan keeps its exact list of cases even when it has a
filter, so a plan does not change under a running test: cases that start or stop matching the filter later
are brought in with **Sync**, which shows the difference first:

```sh
curl -fsS -X POST "$api/test-plans/$plan/sync?dryRun=true" -H "$auth"
```

```json
{"added": [31, 32], "removed": [7]}
```

`added` are cases the filter matches that the plan lacks; `removed` are cases the filter added earlier and no
longer matches. Explicitly added cases are never removed by a sync. Run the same call without `dryRun` to
apply it. After changing the filter (`PATCH $api/test-plans/$plan` with `If-Match`), sync to apply it.

| Call | Does |
|---|---|
| `GET $api/test-plans?q=&archived=` | Managed plans with their counts (total, manual, automated) |
| `GET $api/test-plans/{id}` | A plan, with `ETag` |
| `PATCH $api/test-plans/{id}` | Change `name`, `description` or `filter`; needs `If-Match` |
| `DELETE $api/test-plans/{id}` | Archive the plan; it stays readable and launches keep their link |
| `GET $api/test-plans/{id}/items?folder=&assignee=` | The cases of the plan in order; `assignee` is `me`, `none` or a name |
| `POST $api/test-plans/{id}/items` | `{add: [n], remove: [n], assign: [{number, assignee}]}`, up to 10 000 numbers per request; needs no `If-Match` but raises the plan's revision |
| `GET $api/test-plans/{id}/launches` | The launches started from the plan, newest first, as a list of launches with their `manual` part (`launches:read`; `before`/`limit` page by launch number) |

A plan holds up to 50 000 cases.

### Assignees

Each case of a plan can have an assignee: the tester who runs it. In the plan builder, assign a folder to a
person at once; through the API:

```sh
curl -fsS -X POST "$api/test-plans/$plan/items" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"assign":[{"number":12,"assignee":"alice"},{"number":13,"assignee":"bob"}]}'
```

Assignees are the user names people sign in with (for OIDC, the username claim). The assignee boxes in the
UI suggest the people known to the project (`GET $api/people?q=`, needs `projects:read`, at most 100, as
`{items: [{principal}]}`): users bound to the project in the access file, active managed tokens whose role
can execute runs in it, and every name already used as an assignee or executor in its runs and plans. Members
of a bound group appear only once they have been assigned or have executed something. You can still type any
name; a name that matches nobody known is marked as an unknown user, because work assigned to a wrong name
shows up in nobody's queue.

The plan page also lists the **Launches** started from the plan, with the progress of their manual runs.

## Starting a launch from a plan

**Start launch** on a plan (or the call below, needs `runs:execute`) creates a launch with a **run item** for
every manual case of the plan:

```sh
curl -fsS -X POST "$api/test-plans/$plan/launches" -H "$auth" -H 'Content-Type: application/json' -d '{
  "name": "Release 2.4 regression",
  "externalId": "release-2.4-rc1",
  "environment": {"browser": "firefox"},
  "assign": "plan",
  "includeAutomated": true
}'
```

| Field | Meaning |
|---|---|
| `name` | Launch name (required) |
| `externalId` | The launch's external id; generated when absent. Starting again with the same id returns the same launch (`200`); an id used by a launch of something else answers `409 launch_exists` |
| `environment`, `attributes` | As for any launch |
| `assign` | Required: `plan` (the plan's assignees), `me` (everything to the caller) or `none` |
| `includeAutomated` | Required: whether CI will run the automated cases of the plan into this launch |

The answer holds the `launch`, its `testplanUrl` and a `ciHint`: the shell lines that make a CI job run the
plan's automated cases into this launch. Each run item keeps the version of its case at the moment the run
started, so editing a case later does not change a running test.

Run items can also be added to any launch, for example a nightly CI launch:
`POST $api/launches/{id}/run-items` with `{caseNumbers: [n]}` or `{planId}` and an optional `assignee`. Only
manual cases become run items, whichever way they are named: cases already in the launch and automated cases
are skipped: `skipped` counts them and `skips` names them (`[{caseNumber, code}]` with `automated` or
`in_launch`, the first 1000). A launch holds up to 50 000 run items. On the launch's **Manual** tab,
**Add cases…** does the same for a running launch and says how many were skipped and why.

### The automated part: testplan.json

Allure adapters (and the FastProof [reporters](reporters.md)) run only the tests listed in an Allure
`testplan.json` named by `ALLURE_TESTPLAN_PATH`. For a launch started from a plan, FastProof computes it from
the plan's automated cases at the moment it is read: each entry has the test's `ALLURE_ID` as `id` when the
code has one, and its full name as `selector`; muted tests are also listed in `mutedTests`. In the CI job:

```sh
export FASTPROOF_PROJECT=web FASTPROOF_LAUNCH=release-2.4-rc1
fastproof testplan --launch "$FASTPROOF_LAUNCH" > testplan.json
export ALLURE_TESTPLAN_PATH="$PWD/testplan.json"
# run the tests, then:
fastproof upload allure-results
fastproof launch finish
```

`FASTPROOF_LAUNCH` makes the upload join the launch from the plan. `fastproof testplan --plan <id>` prints the
same file for a plan without starting a launch. The HTTP equivalents are
`GET $api/launches/{id}/testplan.json` and `GET $api/test-plans/{id}/testplan.json` (`launches:read`). A plan
without automated cases answers `409 plan_has_no_automated_cases` instead of an empty file, because an empty
`testplan.json` means "run every test" to Allure adapters.

!!! note "Launches from a plan and code sync"
    A launch started from a plan has no VCS data of its own, and the person who started it usually has no
    `cases:sync` (a `tester` does not). When CI joins the launch with a token that has `cases:sync` (the
    `uploader` role), by `FASTPROOF_LAUNCH`/`--launch` or by uploading results to it, the launch counts as
    uploaded by a sync-capable token and takes the branch, repository and commit of that CI job if it has no
    branch yet (the first branch stays). [Code sync](test-management.md#code-sync) then trusts it like any
    other launch from that branch, and quality gates with `branches` apply to it. Code sync runs when the
    launch is finished and again when its manual run is closed.

## Executing a manual run

Testers find their work in three places:

- **My runs** in the project navigation lists the open run items assigned to them across all launches of the
  project, grouped by launch (`GET $api/run-items?assignee=me`); a click opens the item in the runner;
- the launch list marks launches with a manual part with a **Manual** badge that shows whether the run is
  open or closed and how many manual cases are left;
- **Continue testing** on the launch's **Manual** tab.

The runner shows the queue, then the case: preconditions and steps of the executed version (with the name the
case had in that version), each step with its status, an actual result, a comment and attachments. Every
action is saved at once.

### Steps and the suggested status

Step statuses are `untested`, `passed`, `failed`, `blocked` and `skipped`. Steps are addressed by their path
in the executed version: `1`, `1.2`, `1.2.3`.

```sh
curl -fsS -X PUT "$api/run-items/$item/steps/2" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"status":"failed","actual":"The order page shows *Payment declined*"}'
```

```json
{"itemStatus": "in_progress", "suggestedStatus": "failed", "revision": 4}
```

Step writes need no `If-Match`: the last write wins, and sending the same value again is harmless. Fields left
out keep their values. The first step result moves the item from `untested` to `in_progress`.

The **suggested status** of the case follows from its steps: `failed` when a step failed, else `blocked` when
one is blocked; once every step has a status, `passed` (or `skipped` when nothing passed); otherwise the run is
still `in_progress`. The runner shows the same suggestion as the API, for example "In progress · 2 untested";
finishing a case with untested steps asks whether to mark them skipped first and names the status it would
get then. The status of a parent step follows from its children (the worst one, so a blocked child makes the
parent blocked; untested children are ignored while others have a status), also when `untestedStepsTo:
"skipped"` fills in the untouched steps, and can be set by hand.

### Finishing a case

The item's own status is set with `PATCH`, which needs the item's `If-Match`:

```sh
curl -fsS -X PATCH "$api/run-items/$item" -H "$auth" -H "If-Match: \"r4\"" -H 'Content-Type: application/json' \
  -d '{"status":"failed","comment":"Declined for a valid card","untestedStepsTo":"skipped"}'
```

Item statuses are `untested`, `in_progress`, `passed`, `failed`, `blocked` and `skipped`. A final status
(`passed`, `failed`, `blocked`, `skipped`) records the result in the launch at once (see
[Manual results in history and gates](#manual-results-in-history-and-gates)); setting the item back to
`untested` or `in_progress` removes it again. `untestedStepsTo: "skipped"` marks steps nobody touched as
skipped. `assignee` hands the item to someone else.

On the **Manual** tab, select items with their checkboxes (or all shown) to change many at once:

- **Assign to…** hands the selected items to one person (or unassigns them); a single item can be reassigned
  from its row;
- **Set result…** finishes the selected items as `passed`, `failed`, `blocked` or `skipped` after a
  confirmation; their untested steps become `skipped`.

Each item is written with its own `PATCH`, so an item that someone changed meanwhile is read again and then
written; the summary says how many items changed.

### Attachments, defects and issues

- **Attachments:** `POST $api/run-items/$item/attachments` (raw body, the file name in
  `X-FastProof-Attachment-Name`, at most 50 MiB) stores a file; reference it in `attachmentIds` of a step
  write, up to 20 per step. Phones can attach a photo or video from the camera.
- **Quick defect:** `POST $api/run-items/$item/defect` with `{defectTypeId?, comment?, issue?: {url, key?}}`
  triages the recorded result exactly like [triage](rules-and-triage.md) of an automated result. Testers
  (`runs:execute`) can do this only for items whose current execution they executed; being the assignee does
  not count, so reassigning an item to yourself does not make it yours for this rule. `results:triage` allows
  it for any item, and a `403` names it in `details.required`. The
  item needs a final status first (`409 run_item_not_final`).
- **New issue:** `GET $api/run-items/$item/new-issue-url?tracker=jira|gitlab|github` returns a link that opens a
  new issue in the project's configured issue tracker, prefilled with the case, the failed step, expected and
  actual results and a link back: `{url, prefilled, prefill?}`. For GitLab and GitHub the link opens the
  prefilled new-issue form (`prefilled: true`). For Jira, set the numeric `projectId` and `issueTypeId` on the
  Jira tracker (Project settings → Integrations) to get a filled-in create form
  (`secure/CreateIssueDetails!init.jspa`, which Jira 9 and later may have disabled). Without them the link
  opens Jira's create page (`prefilled: false`) and the UI shows **Copy issue text** to put the title and
  description on the clipboard. Jira answers always carry `prefill: {title, description}`. FastProof never
  creates the issue itself; the tester reviews and submits it in the tracker.

### Rerun

`POST $api/run-items/$item/rerun` starts a new execution of a case: the item is `untested` again with empty
step results and the same case version. It needs the item's `If-Match` and is written to the audit log as
`runItem.rerun` with the previous status and execution. Like a retry of an automated test, the new execution
becomes another attempt of the same result; the previous one stays visible, and the result keeps its old
status until the new execution has a final one. The step results and comments of earlier executions are kept
with the run item for good: status, case version, comment, executor, times, defect, step results and their
attachments of each earlier execution, and who started the rerun and when. They are read with
`GET $api/run-items/$item/executions` (newest first; the runner shows them under **Earlier runs of this
case**) and are not affected by `rawDays` partition drops.

### Keyboard

The runner works without a mouse. `?` shows the keys; single-character keys can be switched off in that
dialog (the arrows, `Enter`, `Ctrl+Enter` and `Esc` keep working). While the focus is in a text field only
`Ctrl+Enter` and `Esc` act.

| Key | Action |
|---|---|
| `j` / `k`, `↓` / `↑` | Next / previous step |
| `p` | Passed |
| `f` | Failed (moves the focus to the actual result) |
| `b` | Blocked |
| `s` | Skipped |
| `u` | Back to untested |
| `c` | Comment on the step |
| `a` | Attach a file to the step |
| `Shift+P` | Pass the remaining steps |
| `Enter` | Next step |
| `n` / `Shift+N` | Next / previous case |
| `d` | Quick defect |
| `Ctrl+Enter` (`⌘+Enter` on macOS) | Finish the case with the suggested status (asks before untested steps become skipped) |
| `?` | Show the keys |
| `Esc` | Leave a text field |

On a phone the runner shows the steps one at a time with a bar of large status buttons at the bottom; the
**Finish** buttons are 44 px tall touch targets as well.

## Combined launches and closing a run

A launch can have an automated part from CI and a manual part at the same time. They have separate life
cycles:

- **CI** finishes the launch as usual (`fastproof launch finish`). The quality gate is evaluated on what is
  recorded at that moment; manual cases that are not finished yet do not count. The launch's duration
  (`durationMs`, and the `duration` gate metric) is that of the automated part; manual work does not extend
  it. The manual part has its own times in `manual.startedAt` (the first item started) and
  `manual.finishedAt` (the last final status). A launch with only manual results takes its duration from its
  start to its finish.
- **Testers** keep working after that. The manual part is `open` until someone closes it with **Close manual
  run** (`runs:execute`):

```sh
curl -fsS -X POST "$api/launches/$launch/manual/close" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"untestedTo":"skipped"}'
```

Closing marks the untested items `skipped` (or leaves them with `"untestedTo": "leave"`) and then:

- a purely manual launch (no automated results or uploads) becomes `finished`;
- the quality gate of a finished launch is evaluated again with the manual results, and notifications go out
  with the event `manualRunClosed` only (see [Notifications](notifications.md#events)): channels that heard
  `launchFinished` when CI finished do not hear it again, and a gate that fails only now is reported in the
  `manualRunClosed` message;
- a launch with automated results is queued for code sync again.

A combined launch that CI has not finished yet stays running; CI's finish evaluates the gate. A closed run
refuses changes to its items (`409 manual_run_closed`) until someone with `plans:write` reopens it
(`POST $api/launches/$launch/manual/reopen`). The launch's `manual` object shows the state and the counts per
status; the **Manual** tab shows progress by status and by assignee.

`GET $api/launches/$launch/manual-report?format=csv|json` exports the run: case, version, status, steps,
executor, times and defects. CSV cells that could start a spreadsheet formula are prefixed with `'`. Run items
and the report show the name of the executed version, so renaming a case later does not change the record of
a run.

**Print report** opens a print view of the whole run (`/p/<key>/launches/<id>/manual/print`), built from the
JSON report and independent of the status filter on the screen. For every item it shows the case, the
executed version and its name, the status, assignee, executor, start and finish times, the item comment and
the defect or issue, and every step with its status, actual result and comment. Print it from the browser or
save it as PDF.

## Manual results in history and gates

A finished manual case becomes an ordinary result of the launch, so everything built on results works for it:
launch counts, the case history, [quality gates](quality-gates.md), dashboards, compare and MCP.

| Run item | Result status | Notes |
|---|---|---|
| `passed`, `failed`, `skipped` | the same | |
| `blocked` | `broken` | The check could not be carried out, which is not a product failure, but gates count it like any broken result (metric `broken`); the original status stays on the run item and in the label `manualStatus` |

A manual result carries the labels `manual=true`, `case=TC-12`, `executedBy` and `manualStatus`; its steps,
actual results and attachments show in the usual step tree. Result lists and the launch tree filter by
`source=manual` or `source=automated`, and manual results are marked with a hand icon. Manual results never
count towards flaky tests.

## Retention

Manual runs are evidence for quality audits, so by default they are kept forever. The project setting
`retention.manualDays` changes that:

```sh
curl -fsS -X PUT "$api/settings" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"retention":{"rawDays":90,"keepImportant":true,"manualDays":730}}'
```

- `manualDays: 0` (the default) keeps launches with a manual run forever. A value from 1 to 36 500 deletes
  launches whose manual run is closed and that were created more than that many days ago.
- `rawDays` and `launchesKeep` never delete a launch with a manual run, and an open manual run is never
  deleted.
- The monthly history rollup keeps launches with a manual run, their run items and step results; the
  recorded results of that month are rolled up into the history like all others.

`retention` is replaced as a whole like every settings area, so send `manualDays` together with the other
retention values.

## Permissions

| Permission | Allows | Roles |
|---|---|---|
| `cases:read` | Read plans | `viewer`, `tester`, `uploader`, `editor` |
| `plans:write` | Create, change, sync and archive plans; reopen a closed manual run | `editor` |
| `runs:execute` | Start launches from plans, add run items, execute and close manual runs, quick defects on items you executed | `tester`, `editor` |
| `launches:read` | Read run items, reports and `testplan.json` | every role |

The `tester` role (`projects:read`, `launches:read`, `cases:read`, `runs:execute`) fits people who execute
runs but do not edit cases or plans. Plan changes, run starts, final statuses, defects, assignee changes and
closing or reopening a run and reruns are written to the audit log (`testPlan.*`, `run.start`, `runItem.update`,
`runItem.rerun`, `manualRun.*`); clicks on single steps are not, they carry who and when on the step result.
