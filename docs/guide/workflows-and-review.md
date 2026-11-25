# Workflows and review

Two optional project settings control how test cases change hands:

- a **case workflow** replaces "any status to any status" with your own states (for example *Draft*,
  *Ready for review*, *Active*, *Outdated*), the transitions people may take between them, and the conditions of
  each transition;
- **version review** makes changes to cases and [shared steps](shared-steps.md) wait for approval: runs, plans
  and `testplan.json` keep using the last approved version while a newer one is reviewed.

Both are off in a new project. They are independent, and work best together: a transition can require an
approved version.

In the examples, `$api` is `https://fastproof.example.com/api/v1/projects/shop` and `$auth` is
`Authorization: Bearer <token>`.

## Case workflow

A case keeps its **status** (`draft`, `active` or `outdated`), which filters, plans, bulk actions, code sync and
`testplan.json` work with. A workflow adds a **state**: the key of one of your statuses, each of which belongs to
one of the three categories. Moving a case to a state also sets its status to the state's category.

```json
{
  "statuses": [
    {"key": "draft", "name": "Draft", "category": "draft"},
    {"key": "ready_for_review", "name": "Ready for review", "category": "draft", "blocksRuns": true},
    {"key": "active", "name": "Active", "category": "active"},
    {"key": "outdated", "name": "Outdated", "category": "outdated", "color": "#868e96"}
  ],
  "manual": {
    "initial": "draft",
    "transitions": [
      {"from": ["draft"], "to": "ready_for_review"},
      {"from": ["ready_for_review"], "to": "active", "requires": ["approved"]},
      {"from": ["*"], "to": "outdated", "permission": "cases:review"}
    ]
  },
  "automated": {"initial": "active", "transitions": [{"from": ["*"], "to": "*"}]}
}
```

This is the suggested workflow that **Settings › Workflow** offers and `GET $api/case-workflow` returns while
the project has none (`"configured": false`).

| Part | Meaning |
|---|---|
| `statuses` | 1 to 20 states. `key` matches `^[a-z][a-z0-9_]{0,31}$` and is unique; `name` has 1 to 100 bytes; `category` is `draft`, `active` or `outdated`, and every category needs at least one state; `color` is `#rrggbb` |
| `blocksRuns` | Cases in this state do not enter manual runs or `testplan.json` (skipped with `blocked_by_state`) |
| `manual`, `automated` | The flow of manual and of automated cases: the `initial` state and up to 100 `transitions` |
| `from`, `to` | State keys; `*` in `from` means any state, `to: "*"` any target |
| `permission` | `cases:write` (default) or `cases:review` |
| `requires` | Up to three guards: `approved` (the head version is approved, or needs no review), `requiredFields` (every required [custom field](test-management.md#custom-fields) has a value), `notAuthor` (the person is not the author of the head version) |

### Moving a case

The state chip in the case header lists the transitions from the current state; the ones whose rule, permission
or guard is missing are shown disabled with the reason. Through the API:

```sh
curl -fsS -X POST "$api/cases/12/transition" -H "$auth" -H 'If-Match: "r7"' \
  -H 'Content-Type: application/json' -d '{"to": "ready_for_review"}'
```

A transition changes `state` and `status` together, raises the revision and sends `case.updated`; it writes a
new [version](test-management.md#versions) only when the category changes. A refused transition answers
409 `invalid_transition` with what is missing:

```json
{"code": "invalid_transition", "message": "…",
 "details": {"from": "ready_for_review", "to": "active", "missing": ["approved"]}}
```

`missing` lists `transition` (no rule leads there), `permission`, `approved`, `requiredFields` or `notAuthor`.
A state outside the `draft` category also needs every required field.

Other ways of changing the status follow the same rules:

- `PUT` and `PATCH` of a case and `setStatus` in [bulk changes](test-management.md#bulk-changes) move the
  case to the state whose key equals the new status, or else to the first state of that category the person may
  move it to;
- bulk `setState` (`{"action": "setState", "value": "active"}`) checks the transition to that exact state for
  every case; cases that may not move are listed in `skipped` with `invalid_transition`;
- code sync, imports and new cases are system changes: they take the flow's `initial` state when it has the
  case's status, else the first state of the status's category, without a check.

Cases that existed before the workflow have no state; they count as being in the state that stands for their
status (the one whose key is the status, else the first of that category) until they move.

### Changing the workflow

`PUT $api/case-workflow` (needs `settings:write`) replaces the workflow; `{"workflow": null}` removes it.
`caseWorkflow` is also part of `PUT $api/settings`.

Removing a state that cases are in, or moving it to another category, is refused with 409
`workflow_status_in_use` and the states with their case counts:

```json
{"code": "workflow_status_in_use", "details": {"statuses": [{"key": "ready_for_review", "cases": 42}]}}
```

Send the new workflow again with `remap`, from each listed state to a state of the new workflow (the UI asks for
it in a dialog):

```sh
curl -fsS -X PUT "$api/case-workflow" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"workflow": {…}, "remap": {"ready_for_review": "draft"}}'
```

The answer carries a `workflow_remap` [job](test-management.md) that moves the cases in batches of 500; a case
whose new state has another category changes its status too, with a version `source = bulk`. Removing the
workflow starts a job that clears every state. One remap per project runs at a time (409 `job_busy`).

## Version review

```json
{"caseReview": {"mode": "manual", "approvals": 1, "resetOn": "content",
                "allowSelfApproval": false, "imports": "review"}}
```

| Setting | Values |
|---|---|
| `mode` | `off` (default), `manual` (manual cases and shared steps), `all` (also automated cases) |
| `approvals` | 1 to 5 distinct people who approve a version |
| `resetOn` | `content`: a change of the name, preconditions, steps or parameters needs a new review; `any`: every change except the status does |
| `allowSelfApproval` | Whether the author of a version may approve it (for teams of one) |
| `imports` | `review`: imported changes wait for review like any other; `bypass`: an import approves what it writes (audited with `reviewBypassed: true`) |

Save it in **Settings › Workflow** or with `PUT $api/settings` (`settings:write`).

### How a version is reviewed

- A change that needs review makes the new head version **pending**. The version runs used before stays the
  **approved version**: a case that was never changed under review counts its previous version as approved; a
  new case has no approved version until someone approves it.
- Changes that need no review (tags, fields or the folder with `resetOn: content`, the status, values code owns
  on an automated case) keep the state; on an approved case the approval moves to the new head.
- People with `cases:review` vote on the **head** version: **approve** or **request changes**, with an
  optional comment (up to 4000 bytes). A later vote of the same person on the same version replaces the earlier
  one. Any request for changes sets the head to `changes_requested` until a new version or until that person
  changes the vote; `approvals` distinct approvals make it `approved`.
- The author of the head does not vote on it (403 `self_approval`) unless `allowSelfApproval`; the principal that
  created an unreviewed AI draft never does (403 `ai_self_review`). AI agents have no `cases:review` and cannot
  vote at all.

Runs started from a plan or by adding cases take each case's **runnable** version: the head when review is off,
when the case is out of scope (an automated case in `manual` mode), or when it was never changed under review;
otherwise the approved version. Cases without one are skipped with `not_approved`; `includeUnapproved: true`
runs the heads instead and needs `cases:review`. Shared steps follow the same rule. Version pruning never
deletes the approved version.

### Review of one case

The **Review** tab of a case shows the state, the votes, and the diff from the approved version to the head.

| Call | Does |
|---|---|
| `GET $api/cases/12/reviews` | The review: `headVersion`, `approvedVersion`, `reviewState`, `author`, `approvals` of `required`, `votes`, whether you may vote (`canVote`, else `whyNot`), and the workflow `transitions` from the case's state. `ETag` is the case revision |
| `POST $api/cases/12/reviews` | A vote `{"version": 4, "decision": "approve", "comment": "…"}` with `If-Match`; needs `cases:review` |
| `GET`, `POST $api/shared-steps/3/reviews` | The same for a shared step |

```sh
curl -fsS -X POST "$api/cases/12/reviews" -H "$auth" -H 'If-Match: "r9"' \
  -H 'Content-Type: application/json' -d '{"version": 4, "decision": "request_changes", "comment": "Step 3 misses the expected result"}'
```

| Answer | When |
|---|---|
| 409 `version_moved` `{currentVersion}` | The version is not the head any more: read the change and vote again |
| 409 `not_pending` | The head is not waiting for review |
| 409 `review_not_enabled` | Review is off, or the case is out of scope |
| 403 `self_approval`, `ai_self_review` | See above |
| 412 | The case changed since you read it |

Every vote sends `case.reviewed` (`sharedStep.reviewed`) with `{number, version, reviewState, approvedVersion}`
to live clients and is audited as `caseReview.vote`.

### The review queue

**Reviews** in the project menu lists what waits for review: `GET $api/reviews` with `kind` (`case` or
`sharedStep`), `state` (`pending` or `changes_requested`), `reviewer=me` (what you may approve and have not voted
on at the head), `author=me` (what you changed), `folder` (a folder and its subfolders), `before` and `limit`.

### Reviewing many changes as one

Batch mode on the **Reviews** page takes the changed cases of a folder, of a test plan or of an
[FPQL query](query-language.md), shows their diffs one after another, and records one decision for all of them,
like approving a merge request:

```sh
curl -fsS -X POST "$api/reviews/bulk" -H "$auth" -H 'Content-Type: application/json' \
  -d '{"items": [{"kind": "case", "number": 12, "version": 4}, {"kind": "sharedStep", "number": 3, "version": 2}],
       "decision": "approve", "comment": "Checkout rework"}'
```

Up to `maxBulkReview` (500) items; each is checked like a single vote, and the ones that fail are listed in
`skipped` with `version_moved`, `self_approval`, `ai_self_review`, `not_pending`, `review_not_enabled` or
`not_found`. It is audited as `caseReview.bulk`.

### Notifications

Subscribe a channel to `caseReviewRequested` to hear about versions waiting for review: see
[Notifications](notifications.md#review-requests).

## Permissions

| Permission | Allows | Roles |
|---|---|---|
| `cases:read` | Read the workflow, reviews and the queue | `viewer`, `tester`, `uploader`, `editor`, `agent` |
| `cases:write` | Transitions whose rule asks for `cases:write` | `editor` |
| `cases:review` | Vote, review in bulk, transitions that ask for `cases:review`, start runs with `includeUnapproved` | `editor` |
| `settings:write` | The workflow and the review policy | `admin` |

Votes, transitions and bulk reviews count towards the per-caller limit of test-management writes
(`FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE`); a bulk review also takes a slot of the heavy request guard.
Changes are audited as `case.transition`, `caseReview.vote`, `caseReview.bulk`, `caseWorkflow.settings` and
`caseReview.settings`.
