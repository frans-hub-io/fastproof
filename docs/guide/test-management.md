# Test cases

FastProof keeps test cases next to the results they produce. A case can be manual, automated or both: the
steps a tester follows and the test in code that checks the same thing are one case with one history. Cases
from code appear by themselves, from launches of trusted branches; manual cases are written in the UI, through
the API or [imported](importing-test-cases.md) from another tool. Plans and manual runs are described in
[Test plans and manual runs](manual-runs.md).

In the UI, open **Test cases** in the project navigation. Every action of the UI is also in the API under
`/api/v1/projects/{key}`; the examples on this page use:

```sh
api=https://fastproof.example.com/api/v1/projects/web
auth="Authorization: Bearer $FASTPROOF_TOKEN"
```

## Concepts

### Case

A case has a **number** that is unique in the project and never reused, shown as `TC-12`. Numbers grow but
may have gaps: a create that fails after its number was taken does not give the number back. The API addresses a
case by its number (`GET $api/cases/12`). A case holds:

| Part | What it is |
|---|---|
| `name` | 1 to 500 bytes |
| `status` | `draft`, `active` or `outdated` (see [Status](#status-and-the-trash)) |
| `folderId` | The folder the case is in, or `null` for the top level; `GET` also returns `folder` with its path |
| `body.description`, `body.preconditions` | Markdown |
| `body.steps` | Steps with `action`, `expected` and `data` (Markdown each), `attachments` and nested `steps`, up to 3 levels and 500 steps in all |
| `body.links` | `{name, url, type}` with `type` `link`, `issue` or `tms`; `http`, `https` and `mailto` URLs |
| `fields` | Values of the project's [custom fields](#custom-fields) |
| `tags` | Up to 50 tags of at most 100 bytes each |

Every step has an `id` (`s_` followed by letters, digits, `_` or `-`) that stays the same across versions, so
version diffs and manual runs follow a step when steps are moved. Leave `id` empty on new steps and the
server assigns one.

A case also tells where it came from (`origin`: `ui`, `code`, `import` or `ai`), whether it is `automated`,
which fields code owns (`codeOwned`), its code **identities** (the first 100, with the count in
`identitiesTotal`) and its external **keys**.

### Case number and Allure ID

The case number is also the case's default **Allure ID**: every case gets the key `allure_id` with its
number. A test in code that carries the `ALLURE_ID` label with that number (Java `@AllureId("12")`, pytest
`@allure.id("12")`, or the `ALLURE_ID` label in other adapters) is linked to `TC-12`. Write the bare number,
not `TC-12`. Keys of other tools (`testrail`, `qase`, `testops`, `external`) are added by imports and make
re-imports idempotent.

### Identities: one case, manual and automated

FastProof already recognises a test across launches by its **identity** (see
[Test identity](concepts.md#test-identity)). A case links to identities:

- **code identities**: tests as uploads name them. A case can have several, for example after a rename or
  when the same test runs as JUnit and as Allure results;
- one **manual identity** (full name `TC-12`), created for the case, under which manual results are recorded.

The history of a case is the history of all its identities, manual and automated together
(`GET $api/cases/12/history`, filters `branch`, `environment`, `source=manual|automated`). A case is
`automated` once a code identity is linked to it; its manual steps stay.

### Folders

Folders form a tree up to 10 levels deep, with at most 20 000 folders per project. Sibling folder names are
unique regardless of case. Each folder shows how many cases it holds with its subfolders, split into manual
and automated.

| Call | Does |
|---|---|
| `GET $api/case-folders?parent=<id>` | One level of the tree (top level without `parent`), with counts |
| `GET $api/case-folders?all=true` | The whole tree in one answer, parents before children, at most `maxFolders` folders (`truncated: true` when there are more) |
| `POST $api/case-folders` `{name, parentId?, position?}` | Create a folder |
| `PATCH $api/case-folders/{id}` `{name?, parentId?, position?}` | Rename, reorder or move with the subtree (`parentId: null` = top level) |
| `DELETE $api/case-folders/{id}?mode=moveUp\|archiveCases` | Delete; a folder with subfolders or cases needs `mode`: `moveUp` moves them to the parent, `archiveCases` moves the cases to the trash |

In the UI, the buttons above the folder tree create, rename, delete, move and reorder the selected folder:
**Move…** opens a dialog to choose the new parent (or the top level), and **Move up** / **Move down** change
its place among its siblings. With a mouse you can also drag a folder onto another folder to move it there.
Everything works from the keyboard: the tree takes arrow keys, and the dialog and buttons replace dragging.

Besides folders, the tree can group cases by their Allure behaviours (epic, feature, story) or by suite:
`GET $api/cases/tree?groupBy=folder|behavior|suite&parent=`.

### Custom fields

Every project starts with built-in fields: `priority` (select: critical, high, medium, low), `severity`
(select: blocker, critical, normal, minor, trivial), and the text fields `layer`, `owner`, `epic`, `feature`,
`story`, `suite` and `component`. Admins (`settings:write`) add up to 100 fields of the types `select`,
`multiselect`, `text`, `number` and `date` (`yyyy-mm-dd`).

In the UI, fields are managed on the **Case fields** page: open **Project settings → Case fields**, or the
**Fields** button on the **Test cases** page. There you can:

- create a field with its key, name, type, options, required flag, default value and source label;
- edit a field's name, options (add, recolour, archive or restore an option), required flag, default value and
  source label;
- archive a field and restore it later (switch on **Show archived** to see archived fields);
- change the order with **Move up** and **Move down**; the order is the order of fields on a case.

The same through the API:

```sh
curl -fsS -X POST "$api/case-fields" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"key":"platform","name":"Platform","type":"multiselect",
       "options":[{"value":"web"},{"value":"ios"},{"value":"android"}],"required":false}'
```

| Field | Meaning |
|---|---|
| `key` | `^[a-z][a-z0-9_]{0,62}$`, unique in the project, used in `fields` and filters |
| `type` | Cannot be changed after the field is created |
| `options` | Values of `select` and `multiselect`, up to 1000; archive an option instead of removing it |
| `required` | A value is needed before a case leaves `draft`; [imported](importing-test-cases.md#required-fields-and-the-status-of-imported-cases) cases without it stay `draft` |
| `sourceLabel` | An Allure label whose value fills the field from code (see [Code sync](#code-sync)) |

`DELETE $api/case-fields/{id}` archives a field: the values on cases are kept, and `PATCH {"archived": false}`
restores it.

### Tags

Tags are free text. Code sync fills them from the `tag` labels of automated tests.

### Status and the trash

The life cycle is fixed: `draft` (being written; required fields may be empty), `active` (in use) and
`outdated` (no longer relevant, kept for history). Any status can change to any other.

Deleting a case moves it to the **trash** (`DELETE $api/cases/12`); `POST $api/cases/12/restore` takes it
back. Archived cases are left out of lists and plan filters; `GET $api/cases?archived=true` lists the trash.
`DELETE $api/cases/12?purge=true` deletes a case with its versions for good; it needs `settings:write` and is
refused (409 `case_has_runs`) for a case that was executed in a manual run, because run results are evidence.

### Versions

Every change of the content (name, status, folder, description, preconditions, steps, links, fields, tags)
writes a new version with a full snapshot. By default versions are kept for the life of the case; the
project's retention settings can prune old ones:

```sh
curl -fsS -X PUT "$api/settings" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"retention":{"rawDays":90,"keepImportant":true,"caseVersionsKeep":20,"caseVersionsDays":365}}'
```

- `caseVersionsKeep` (0 to 100 000, default `0`): keep at least the newest this many versions of each case.
- `caseVersionsDays` (0 to 36 500, default `0`): versions older than this many days may be pruned.
- With both set, a version is pruned only when it is beyond the newest `caseVersionsKeep` **and** older than
  `caseVersionsDays`; with one set, that one decides; with neither (both `0`), nothing is pruned.
- Never pruned: the current version, the versions that run items (and their earlier executions) were run
  against, so manual runs always show the steps that were executed, a version replaced less than an hour ago
  (a launch being started may still record it), and on cases made from code the newest version that a person
  or an import wrote.

The hourly job `cases.versions.prune` (on the `scheduler` role) deletes them in batches of 1000, at most 5
minutes per run, starting at another project each hour; the next run goes on. `retention` is replaced as a whole like every settings area, so send
the other retention keys too. A pruned version is missing from the list and answers `404`.

| Call | Does |
|---|---|
| `GET $api/cases/12/versions` | Versions, newest first: `version`, `source`, `changed` (the parts that changed), who and when, and `launchId` for changes from code |
| `GET $api/cases/12/versions/3` | The snapshot of version 3 |
| `POST $api/cases/12/versions/3/restore` | Makes version 3 current as a new version (`source = restore`); needs `If-Match` |

`source` tells where a version came from: `ui`, `code`, `import`, `ai`, `restore` or `bulk`. The UI shows a
diff of any two versions, field by field and step by step. Moving a case to the trash and back, and moving
cases out of a deleted folder, do not write versions. Restoring a version keeps the values that code owns on
an automated case, and puts the case at the top level when its old folder no longer exists.

## Writing cases

Descriptions, preconditions and step texts are [CommonMark](https://commonmark.org/) Markdown with tables and
strikethrough. The server stores the Markdown as written and renders it itself, the same way for the UI,
exports and MCP:

- raw HTML is shown as text, never as markup;
- links work for `http`, `https` and `mailto` only;
- images must be attachments of the case: upload a file with `POST $api/cases/12/attachments` (raw body, the
  file name in `X-FastProof-Attachment-Name`) and write `![screenshot](attachment:<id>)`. The UI does this
  when you paste an image. Other image sources show their alt text.

One Markdown field holds at most 16 KiB and the whole body at most 1 MiB; a case keeps up to 100 attachments
of 50 MiB each (see [Limits](../operations/limits.md#test-management)). `POST $api/markdown/preview`
`{"markdown": "..."}` renders a text the way a case shows it.

Create a case:

```sh
curl -fsS -X POST "$api/cases" -H "$auth" -H 'Content-Type: application/json' -d '{
  "name": "Pay with a saved card",
  "status": "draft",
  "body": {
    "description": "The customer pays with a card stored in the profile.",
    "preconditions": "A signed-in customer with one saved card and a non-empty cart.",
    "steps": [
      {"action": "Open the cart and choose **Checkout**", "expected": "The payment page lists the saved card"},
      {"action": "Choose the saved card and confirm", "expected": "The order page shows *Paid*",
       "data": "Card ending in `4242`"}
    ],
    "links": [{"name": "Payment spec", "url": "https://example.com/specs/payments", "type": "link"}]
  },
  "fields": {"priority": "high", "platform": ["web"]},
  "tags": ["checkout", "payments"]
}'
```

`name` and `body` are required; `body` needs all four parts (`description`, `preconditions`, `steps`,
`links`), which may be empty. The answer is `201` with the case, its rendered HTML (`html`) and an `ETag`
header. `GET` returns the case with `folderId` (and `folder` with its path). `PUT $api/cases/12` replaces the content; `folderId`, fields, tags and status left out of the body keep
their values (send `"folderId": null` to move the case to the top level), so a case read with `GET` and sent
back changed stays in its folder. `PATCH` merges `folderId`, `status`, `fields` (`null` removes a value) and
`tags`.

### Finding cases

`GET $api/cases` lists cases with filters, all combinable:

| Parameter | Matches |
|---|---|
| `folder`, `recursive` | Cases of a folder, with `recursive=true` also of its subfolders |
| `q` | Words of the name, description, preconditions or steps, or a number (`TC-12`) |
| `status` | `draft`, `active`, `outdated`; repeated or comma-separated |
| `automation` | `manual` or `automated` |
| `field=key:value` | A field value; repeated: the same key matches any of the values, different keys must all match |
| `tag` | A tag; repeated: all of them |
| `staleDays` | Automated cases without a trusted result for this many days |
| `aiUnreviewed` | Cases an AI agent created that nobody has reviewed yet |
| `archived=true` | The trash |
| `sort` | `number` (default), `-number` or `updated` |

```sh
curl -fsS "$api/cases?folder=$folder&recursive=true&status=active&field=priority:high&tag=checkout" -H "$auth"
```

### Bulk changes

`POST $api/cases/bulk` applies one action to up to 10 000 cases, named by `numbers` or matched by a `filter`:

```sh
curl -fsS -X POST "$api/cases/bulk" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"filter":{"automation":"automated","tags":["legacy"]},"action":"setStatus","value":"outdated"}'
```

Actions: `move` (a folder id or `null`), `setStatus`, `setField` (`{"key", "value"}`), `addTags`,
`removeTags` (a list), `archive`, `restore`. Each changed case gets one version with `source = bulk`; cases
that cannot change are listed in `skipped` with a code. Bulk changes need no `If-Match`: the last writer wins.

## Editing at the same time

Two people, or a person and code sync, can change the same case. FastProof never overwrites a change
silently:

- `GET` of a case returns `ETag: "r<revision>"`; every change raises the revision.
- `PUT` and `PATCH` of a case, and the restore of a version, need `If-Match` with that value. Without it the
  answer is `428 precondition_required`.
- When the case changed since it was read, the answer is `412 precondition_failed` with
  `details: {currentRevision, changedBy, source}`, and nothing is saved.

```sh
rev=$(curl -fsS "$api/cases/12" -H "$auth" | jq .revision)
curl -fsS -X PATCH "$api/cases/12" -H "$auth" -H "If-Match: \"r$rev\"" -H 'Content-Type: application/json' \
  -d '{"status":"active"}'
```

For a person in the UI a 412 means: someone else saved first. The editor shows the current version next to
the result of saving yours and lets you save your changes, take the current version or go on editing; nothing
you typed is lost. **Save my changes** applies only what you changed (each top-level part such as the name,
status, folder, description or tags, each custom field by its key, and each step by its id) on top of the
current version, so the other writer's changes to other parts are kept. The dialog names who saved the
current version and how; when it was you in another tab or through the API, it says "you (another tab)".
When `source` is `code`, the change came from [code sync](#code-sync). An open case also shows a banner as
soon as another change arrives (live event `case.updated`), naming who made it the same way. The same
`ETag`/`If-Match` rule applies to plans and run items.

## Code sync

Cases follow the automated tests: when a **trusted** launch is finished, FastProof creates a case for each new
test, links tests that carry a known Allure ID or TMS key, and updates the values that code owns. This runs in
the background (job `cases.sync`, every 10 s), not while results are uploaded, so it never slows ingest.

### Trusted launches

A launch is trusted when all of these hold:

- it is not a debug launch (unless `includeDebug` is on);
- its branch matches one of `branches`, or equals the CI default branch when `useCiDefaultBranch` is on (the
  CLI reports it as `vcs.defaultBranch` from `CI_DEFAULT_BRANCH` in GitLab or the event payload in GitHub
  Actions; Jenkins does not provide it);
- the caller that created the launch had the `cases:sync` permission;
- every upload and results batch in it also came from callers with `cases:sync`.

When a caller without `cases:sync` (for example a merge-request token with the `mr-uploader` role below)
uploads or posts results into a launch, the launch is reported as usual but becomes untrusted for good: its
tests stay unlinked and it changes no case, even if a syncing token adds to it later. A finished launch
accepts uploads, results and a second finish only from the caller that created it or a caller with
`cases:sync`; anyone else gets `409 launch_finished`. Joining a launch by `externalId` still works for every
caller with `launches:write`.

An untrusted launch changes nothing in cases: its new tests stay **unlinked** and appear on the
**Unlinked tests** tab with the branches they were seen on (`GET $api/test-identities?unlinked=true`), where
you can create a case for one or link it to an existing case. Feature branches and experiments therefore
never flood the repository.

!!! tip "Merge request pipelines"
    `cases:sync` is part of the `uploader` and `editor` roles. Give pipelines of merge requests and forks a
    token without it, so their results are reported as usual but never change cases. If such a token posts
    into a launch that a trusted pipeline is still running, that launch is reported as usual but no longer
    syncs: its new tests stay unlinked until the next clean pipeline. No built-in role
    uploads without `cases:sync`; define one in the [access file](../operations/security.md#access-file):

    ```yaml
    roles:
      - name: mr-uploader
        permissions: ["projects:read", "launches:read", "launches:write", "cases:read"]
    ```

    and create the merge request token with `"role": "mr-uploader"`.

### Settings

Code sync is configured in the `caseSync` area of the project settings (`PUT $api/settings`, needs
`settings:write`). These are the defaults:

```json
{"caseSync": {"enabled": true, "branches": ["main", "master"], "useCiDefaultBranch": true,
  "includeDebug": false, "createCases": true, "folderFrom": "suite", "rootFolder": "Automated",
  "staleAfterDays": 30, "matchTmsLinks": false,
  "fields": {"name": "code", "description": "code", "links": "code", "tags": "code", "epic": "code",
             "feature": "code", "story": "code", "suite": "code", "severity": "code", "owner": "code",
             "layer": "code", "component": "code"}}}
```

| Setting | Meaning |
|---|---|
| `enabled` | Code sync on or off for the project |
| `branches` | Trusted branches, at most 50 globs: `*` matches within one path segment, `**` any number of segments (`release/*` matches `release/2.4` but not `release/2.4/hotfix`; `**/hotfix` matches both `hotfix` and `team/a/hotfix`) |
| `useCiDefaultBranch` | Also trust the default branch the CI reports |
| `includeDebug` | Also trust debug launches |
| `createCases` | Create cases for new tests; `false` only links and updates |
| `folderFrom` | Where new cases go below `rootFolder`: `suite` (one folder per level of the result's suite path: the Allure labels `parentSuite`, `suite`, `subSuite`, else the class part of the full name), `package` (the package of the full name, for example `com.shop` of `com.shop.CartTest.adds`) or `none` |
| `rootFolder` | Top folder of cases created from code |
| `staleAfterDays` | Days without a trusted result after which an automated case counts as stale |
| `matchTmsLinks` | Link tests to cases by the TestRail or Qase keys of their `tms` labels and TMS links (useful after an import) |
| `fields` | Who owns each value on automated cases: `code` or `case` (see below) |

Unlike other settings areas, `caseSync` is merged key by key: a `PUT $api/settings` with
`{"caseSync": {"branches": [...]}}` changes only `branches` and keeps the other settings (`fields` is merged
by field name too). `null` for a key returns it to its default, and `"caseSync": null` resets the whole area.
Branch globs here differ from [quality gate](quality-gates.md) branches, where `*` also matches `/`.

### Who owns a value: code or case

For each of `name`, `description`, `links`, `tags` and the fields `epic`, `feature`, `story`, `suite`,
`severity`, `owner`, `layer` and `component`, the field policy decides:

- `code`: the value comes from the latest trusted result (its labels, description and links). In the UI the
  value is locked with a "from code" badge; writing it through the API answers
  `409 field_from_code` with the names in `details.fields`. Change it in the code. A value the result does
  not have (no description, no `tag` labels, no `epic` label) never clears the case's value: the case keeps
  what it had.
- `case`: code fills the value only when it creates the case; after that the case owns it and people edit it.

Steps, preconditions, the folder, the status and fields without a `sourceLabel` always belong to the case.
A custom field with a `sourceLabel` that is not one of the names above is owned by code. Code sync writes a
new version only when the values from code changed (`source = code`, with the launch), and raises the
revision, so an open editor learns about it.

### Automating a manual case

To automate a manual case, give the new test in code the case number as its Allure ID:

```java
@Test
@AllureId("12")
void paysWithSavedCard() { ... }
```

The first trusted launch with this test links it to `TC-12`. The case becomes automated and keeps its manual
steps and preconditions. On this first link the manual text wins: code fills only the values the case does
not have yet (an empty description, no tags, an empty label field), and the name, description and other
values people wrote stay. From then on the values that code owns follow the test whenever they change in the
code, and the previous ones stay in the versions. Set the policy of a value to `case` if it should never
follow the code.

This also works for a test that code sync already gave a case of its own (for example the backfill after an
upgrade created `TC-305` for it): when its `ALLURE_ID` names another case, the next trusted launch moves the
test to `TC-12`. The case created from code goes to the trash with a link to `TC-12` when nothing else holds
it: it was created by code, has no other test, was never run manually and was never edited (all its versions
come from code). Otherwise both cases stay and only the test moves; the old case is no longer automated
unless another test is linked to it. Each move is written to the audit log as `case.linkIdentity` with
`source: code`.

### Renames and duplicates

Renaming a test or moving it to another class changes its identity, so code sync sees a new test and creates
a second case. Link the new identity to the original case: on the case's **Details** tab, or

```sh
curl -fsS -X POST "$api/cases/12/identities" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"testCaseId":"0193a5b2-7c1d-7e3f-9a0b-1c2d3e4f5a6b"}'
```

When the identity was the last one of another case, that duplicate goes to the trash with a link to `TC-12`.
`DELETE $api/cases/12/identities/{testCaseId}` unlinks one. Tests with an explicit id keep their identity
across renames (see [Test identity](concepts.md#test-identity)). A case lists its first 100 identities;
`identitiesTotal` tells how many it has.

### Stale tests

Cases are never deleted because their test disappeared. `GET $api/cases?automation=automated&staleDays=30`
lists automated cases without a trusted result for 30 days; mark them `outdated` with a bulk change.

## After an upgrade: cases for existing tests

When a server is upgraded to the release with test management, a background job (`cases.backfill`) gives a
case to every existing test with results on a trusted branch, using the same rules and folders as code sync,
once per project that has launches from before the upgrade. It waits ten minutes after the upgrade
(`FASTPROOF_TM_BACKFILL_DELAY`, for example `30m`, `2h` or `0`; see
[Limits](../operations/limits.md#test-management)) and then decides per project: a project whose
`caseSync.enabled` is `false` by then is skipped for good, so turn code sync off for projects that should get
no cases from code within that time. Case numbers follow the order in which the tests
were first seen, and an existing `ALLURE_ID` becomes a key. Launches from before the upgrade count as created
with `cases:sync`, since the permission did not exist yet; they carry no CI default branch, so only
`branches` decide. Tests seen only on other branches stay unlinked. The job works in batches of 1000 tests,
runs on the scheduler replicas and continues after a restart; 100 000 tests take less than ten minutes.

When a project's main branch is not called `main` or `master`, set `caseSync.branches` and start the backfill
again (needs `settings:write`). Only `branches` changes; the other `caseSync` settings keep their values. The
backfill only adds cases for tests that are still unlinked, so running it again is safe:

```sh
curl -fsS -X PUT "$api/settings" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"caseSync":{"branches":["develop","release/*"]}}'
curl -fsS -X POST "$api/cases/backfill" -H "$auth"
```

The answer is `202` with the job (`kind: backfill`, `status`, `progress`); its progress also arrives as the
live event `tmJob.updated`. A project skipped after the upgrade gets its backfill only from this call. To
keep a project without cases from code, set `"caseSync": {"enabled": false}`; cases the backfill already
created can be moved to the trash with a bulk `archive`.

## AI drafts and suggestions

Agents connected through [MCP](mcp-copilot.md#test-case-tools) can create cases only as drafts
(`origin = ai`, marked **AI draft** until reviewed) and can change an active case only by a suggestion that a
person accepts or rejects.

Drafts and suggestions of an agent need a second principal, on REST and MCP alike. A case with `origin: ai`
that is still `aiUnreviewed` can be marked reviewed or set to `active` (PUT, PATCH, bulk `setStatus`,
accepting a suggestion, restoring a version) only by a principal other than the one that created it; that
principal gets `403 ai_self_review` (a bulk change skips the case with that code). Activating it by someone
else counts as the review and clears `aiUnreviewed`. A suggestion is never accepted by the principal that
made it (`403 ai_self_review`). The principal is the user name or the token name, so give the agent its own
token.

| Call | Does |
|---|---|
| `POST $api/cases/12/review` | Marks an AI draft as reviewed (clears `aiUnreviewed`, the **Reviewed** button); needs `cases:write` and the case's `If-Match`; no new version, audited as `case.review` |
| `GET $api/cases/12/suggestions?status=pending` | Suggestions for the case |
| `POST $api/case-suggestions/{id}/accept` | Applies it as a version with `source = ai`; needs the case's `If-Match`; a case that changed since answers `409 suggestion_stale` |
| `POST $api/case-suggestions/{id}/reject` | Rejects it; it stays in the history |

A case holds at most 20 pending suggestions; a further one answers `409 limit_reached`
(`maxPendingSuggestions`) until some are accepted or rejected.

## Permissions

| Permission | Allows | Roles |
|---|---|---|
| `cases:read` | Read cases, folders, fields, history and plans | `viewer`, `tester`, `uploader`, `editor`, `agent` |
| `cases:write` | Create, change and archive cases and folders | `editor` |
| `cases:import` | Import cases | `editor` |
| `cases:sync` | Update cases from code through uploaded results | `uploader`, `editor` |
| `ai:propose` | AI drafts and case suggestions through MCP (not the REST case routes) | `agent` |
| `settings:write` | Custom fields, `caseSync` settings, purge, backfill | `admin` |

See [Roles and permissions](../operations/security.md#roles-and-permissions) for all roles. Every change is
written to the audit log (`case.*`, `caseFolder.*`, `caseField.*`, `caseSuggestion.*`, `caseSync.settings`).
