# Requirements and coverage

A **requirement** is something the product must do: an epic, a story, a rule from a specification, a risk. In
FastProof requirements are their own objects (`REQ-12`), arranged in a tree, linked to the
[test cases](test-management.md) that check them. **Coverage** shows, for any requirement, whether its cases
exist, ran and passed, so a release review can answer "what of this epic is tested?" without a spreadsheet.

An issue tracker can be the source of requirements (Jira, GitLab, GitHub, Azure DevOps): FastProof imports
issues as requirements and follows their state. It never writes to the tracker.

The examples on this page use:

```sh
api=https://fastproof.example.com/api/v1/projects/web
auth="Authorization: Bearer $FASTPROOF_TOKEN"
```

## The requirement tree

Open **Requirements** in the project menu. Each requirement has:

| Field | What it holds |
|---|---|
| Number | `REQ-<n>`, given by FastProof and never reused |
| Title | 1 to 500 bytes |
| Description | Markdown, up to 16 KiB (the case limit); raw HTML is never rendered |
| Type | a word you choose: `requirement` (default), `epic`, `story`, `risk`, ... (`^[a-z][a-z0-9_-]{0,31}$`) |
| Status | `draft`, `active` (default), `done`, `obsolete` |
| Parent | another requirement; at most 6 levels deep |
| Tracker issue | optional: tracker id, issue key and URL (an `http`/`https` link) |

Requirements nest: an epic holds stories, a story holds detailed rules. Move a requirement with everything under
it by changing its parent; moving it under itself or one of its children is refused with 409
`requirement_cycle`, and a move that would make the tree deeper than allowed with 409 `limit_reached`.

```sh
curl -s -H "$auth" -H 'Content-Type: application/json' "$api/requirements" \
  -d '{"title": "Checkout", "type": "epic"}'
curl -s -H "$auth" -H 'Content-Type: application/json' "$api/requirements" \
  -d '{"title": "Pay by card", "type": "story", "parentNumber": 1,
       "trackerId": "jira", "issueKey": "SHOP-42", "url": "https://tracker.example.com/browse/SHOP-42"}'
```

A requirement saves like a case: `GET` returns an `ETag`, and `PUT` or `PATCH` need it in `If-Match` (412 when
someone else saved meanwhile, 428 without it). `PUT` replaces every field (fields you leave out take their
defaults, so the requirement moves to the top level and loses its tracker issue); `PATCH` changes only what you
send. An issue key is unique per tracker in a project (409 `issue_key_taken`).

**Delete** archives a requirement **with everything under it**; their links stay. **Restore** brings back what
was archived with it (a requirement whose parent is still archived is refused: restore the parent). Archived
requirements are read only and count nowhere.

Lists:

- `GET /requirements/tree?parent=REQ-1` lists the direct children (top level without `parent`), by number;
  `hasChildren` tells the page whether to offer a fold.
- `GET /requirements` lists by number with filters: `q` (a number or words of the title, description or issue
  key), `parent` (anything under that requirement), `status`, `type`, `query` (FPQL, below), `archived=true`
  (the trash).

## Linking cases

A requirement is **covered** by the cases linked to it. Open a requirement and use **Link cases**, or the
**Requirements** tab of a case. In the API:

```sh
curl -s -H "$auth" -H 'Content-Type: application/json' "$api/requirements/2/links" \
  -d '{"add": [12, 13], "remove": [9]}'
```

`add`, `remove` and `confirm` take case numbers (an unknown case is 404). A requirement has at most 500 links
(409 `limit_reached`). `GET /requirements/{n}/cases` lists its cases with each case's last result;
`GET /cases/{n}/requirements` lists the requirements of a case.

Links come from three places:

- **People** (`origin: ui`, or `import` for case imports). They stay until a person removes them.
- **Code** (`origin: code`). A trusted launch (the conditions of [case sync from code](test-management.md))
  links an automated case to requirements its results name:
    - an Allure link whose type is in the project's `caseSync.requirementLinkTypes` (default `["requirement"]`,
      up to 10 types, compared without case), by the link name or else the last segment of its URL;
    - the label `requirement` (several keys separated by commas).

  A key is `REQ-<n>` or the issue key of an existing requirement that is not archived (`SHOP-42`). Unknown keys
  create nothing. When the code no longer names a requirement, the next trusted sync removes that link. A person
  who adds a link that came from code takes it over: it becomes a manual link and stays.

  ```java
  @Link(name = "REQ-12", type = "requirement")
  @Test void paysByCard() { ... }
  ```

  ```python
  @allure.link("SHOP-42", link_type="requirement")
  def test_pays_by_card(): ...
  ```

- **AI proposals** (`origin: ai`, `linkStatus: proposed`). An MCP agent may only propose links. A proposal does
  not count for coverage until a person **confirms** it with one click (or `{"confirm": [12]}`); removing it
  rejects it.

Changing a custom `caseSync.requirementLinkTypes` is a new settings format: while replicas of an older
version run, it is refused with 409 `upgrade_in_progress` (see [upgrading](../operations/upgrading.md)).

## Coverage

Open **Coverage** in the project menu. Every row is a requirement **with everything under it**: the distinct
cases (not archived) actively linked to it or to any requirement below it. Pick the **scope**, the results that
count:

| Scope | Parameters | A case counts with |
|---|---|---|
| Default | none | its last result (the newest over all its identities, manual or automated) |
| Plan | `planId`, optionally `launchId` (a launch of that plan) | the plan's newest launch, or the launch given |
| Launches | `launchIds` (comma-separated, at most 50) | its newest result in those launches |
| Window | `days` (1 to 365; 30 when only a branch or environment is given), `branch`, `environment` | its newest result in launches of that window (debug launches left out) |

In the plan, launches and window scopes a parameterized case counts with the newest result **per parameter
set**, and of several sets with the worst: one failing browser fails the case. Only one kind of scope may be
given at a time (400 otherwise); a plan or launch of another project is 404.

The status of a requirement follows from its cases:

| Status | When |
|---|---|
| `uncovered` | no active links to cases |
| `failed` | a case failed, broke or was blocked |
| `passed` | every case passed |
| `notRun` | no case passed (none ran, or they were skipped) |
| `partial` | some passed, the rest did not run or were skipped |

Each row also counts the cases (`total`, `passed`, `failed`, `notRun`, `skipped`) and marks
**changed after run** when the requirement's title or description changed after the newest result of its cases:
the tests may no longer check what the requirement says.

```sh
curl -s -H "$auth" "$api/coverage?planId=0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"
curl -s -H "$auth" "$api/coverage?parent=REQ-1&branch=main&days=14"
```

The page lists the children of `parent` (top-level requirements without it) by number, 50 at a time with
`before`/`next`. Expand a row to drill down. A scope other than the default reads results and counts as a heavy
read: a person runs a few of them at a time (429 or 503 beyond that, retry shortly).

### Exports

**Export** (CSV or XLSX) writes the coverage of every requirement in the scope, in tree order: key, title, type,
status, depth, parent, coverage, the counts, changed after run, issue key and external state. It runs as a job:

```sh
curl -s -H "$auth" -H 'Content-Type: application/json' "$api/coverage/exports" \
  -d '{"format": "xlsx", "branch": "main", "days": 30}'
```

The answer's `downloadUrl` is a one-time link for you, valid for 24 hours once the job is `done`
(`GET /tm-jobs/{id}` shows its state). A person has at most 2 and a project at most 4 coverage exports queued or
running (409 `export_busy`). Cells that start with `=`, `+`, `-` or `@` are written with a leading `'` in CSV,
so spreadsheets do not run them as formulas.

### Dashboard widget

The **Requirement coverage** widget of [dashboards](dashboards.md) shows how many top-level requirements are
uncovered, failing, not run, partly or fully passed, and the first ones (its **limit**, default 10). Without
days, branch or environment it uses the default scope; with any of them a window. It is shown to people who may
read test cases (`cases:read`); others see a note instead. A dashboard holds at most two of them. Saving a
dashboard with this widget is a new format and waits for every replica to be upgraded (409
`upgrade_in_progress` meanwhile).

## Searching with FPQL

The [query language](query-language.md) works on requirements too (`GET /requirements?query=`, and
`POST /query/validate` with `"entity": "requirements"`):

| Attribute | Values | Example |
|---|---|---|
| `number` | `12` or `REQ-12` | `number > 100` |
| `title` | text; `~` matches a part | `title ~ "card"` |
| `type` | text | `type in [epic, story]` |
| `status` | `draft`, `active`, `done`, `obsolete` | `status != obsolete` |
| `parent` | the requirement directly above | `parent = REQ-3`, `parent is empty` |
| `issue` | the tracker issue key | `issue is empty` |
| `externalState` | the tracker's state name | `externalState = "In Progress"` |
| `coverage` | `uncovered`, `failed`, `notRun`, `partial`, `passed` | `coverage in [failed, uncovered]` |

`coverage` here is the default scope. Cases can be searched by requirement as well: `requirement = REQ-12`.
A search (`q` or `query`) is a heavy read and stops after 5 seconds.

## Importing from a tracker

Requirements can come from an issue tracker configured in **Settings → Issue trackers** with API access (the
same connection that creates defects, its secret and the egress allow list). **Import from tracker** on the
Requirements page asks for:

- the tracker,
- a **query**: JQL for Jira (`project = SHOP AND issuetype = Story`), a label for GitLab and GitHub
  (`requirement`), WIQL for Azure DevOps (`SELECT [System.Id] FROM WorkItems WHERE [System.WorkItemType] = 'User Story'`),
- optionally a parent requirement for the new ones.

The query is always limited to the tracker's configured project: a JQL becomes
`project = "SHOP" AND (<your JQL>)` (its `ORDER BY` stays at the end), a WIQL gets
`[System.TeamProject] = @project` added to its conditions, and labels are read from the project's own issues.
A JQL or WIQL whose quotes or parentheses do not balance, or a WIQL that is not a flat
`SELECT … FROM WorkItems [WHERE …] [ORDER BY …] [ASOF …]`, is refused (400).

The import is a job with a **dry run**: it reads at most 1000 issues of the project, then stops at `ready` with
how many requirements it would create, update or leave. **Apply** writes them:

```sh
curl -s -H "$auth" -H 'Content-Type: application/json' "$api/requirements/import" \
  -d '{"trackerId": "jira", "query": "project = SHOP AND labels = requirement", "parentNumber": 1}'
curl -s -H "$auth" -X POST "$api/requirements/import/<job id>/apply"
```

New issues become requirements of origin `tracker` with the issue's title, description (as text), URL and
state. Issues are matched by tracker and issue key, so importing again updates instead of duplicating. A dry run
nobody applies is cancelled after 7 days, and a project keeps at most 3 dry runs waiting: a new one cancels the
oldest. Importing needs `cases:import`, and so does reading the job; one requirement import per project runs at a
time (409 `import_busy`).

**State sync.** Every requirement with a tracker issue is read back at most once an hour (or at the tracker poll
interval when that is longer), with the batched reads of [issue tracker sync](issue-trackers.md): the issue's
state becomes the requirement's **external state**, and a new title replaces the requirement's title (which
counts as a content change). Nothing flows the other way.

## Permissions

| Action | Permission |
|---|---|
| Read requirements, links and coverage; export coverage | `cases:read` |
| Create, change, archive requirements; link cases; confirm proposals | `cases:write` |
| Import from a tracker | `cases:import` |
| Links from code | `cases:sync` of the uploading token, on trusted launches |
| AI proposals | `ai:propose` (MCP) |

Every change is in the audit log: `requirement.create`, `requirement.update`, `requirement.archive`,
`requirement.restore`, `requirement.links`, `requirement.import`, and `export.create` with `kind: coverage`.
Pages follow changes live (`requirement.updated`).

## Limits

| Limit | Default | Setting |
|---|---|---|
| Requirements per project | 50 000 | `FASTPROOF_TM_MAX_REQUIREMENTS` |
| Depth of the tree | 6 | `FASTPROOF_TM_MAX_REQUIREMENT_DEPTH` |
| Links per requirement | 500 | `FASTPROOF_TM_MAX_REQUIREMENT_CASES` |
| Requirement keys one result names | 50 | fixed |
| Issues per tracker import | 1000 | fixed |
| Launches of a `launchIds` scope | 50 | fixed |
