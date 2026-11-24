# Shared steps

A **shared step** is a short sequence of steps that many [test cases](test-management.md) reuse, such as "log in
as a customer" or "add an item to the cart". You write it once, give it a number (`SS-12`), and cases
reference it. Change the shared step and every case that uses it follows, without opening each case.

The examples on this page use:

```sh
api=https://fastproof.example.com/api/v1/projects/web
auth="Authorization: Bearer $FASTPROOF_TOKEN"
```

## How a case uses a shared step

A case holds a **reference** to the shared step in its step list, not a copy. The reference has no text of its
own; the case page shows it as a collapsed block with the number, the name and the version (`SS-12 v4`).
Expand the block to read the steps.

- A case references a shared step **without a version**. It always runs the current version.
- A shared step can sit **between the case's own steps**, any number of times (up to 50 references in one
  case).
- References are **top level only**. A shared step cannot contain another shared step, and its own steps nest
  at most two levels.
- A shared step belongs to **one project**. A reference to a shared step of another project is refused with
  404.

To insert one while editing a case, press **Ctrl+Shift+S** (or **Insert shared step**), search by `SS-` number
or by name, and pick it. In the API the step looks like this:

```json
{"id": "", "shared": {"id": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b", "number": 12}}
```

The reference has empty `action`, `expected`, `data`, `attachments` and `steps`; anything else is 400
`invalid_case`. `GET /cases/{n}` lists the referenced shared steps with their steps and rendered HTML in
`sharedSteps`, at the version a run would use now.

## Versions

Every save of a shared step is a **version** (`v1`, `v2`, ...) with the keys that changed (`name`,
`description`, `steps`), who saved it and when. Open **Versions** on the shared step page to compare two
versions side by side and to **restore** an older one, which saves its content as a new version.

Saving a shared step **never creates case versions**. Instead, the **Versions** tab of each case that uses it
has a **Shared step changes** list, so you can see why a case behaves differently than it did last week.

A shared step saves like a case: send `If-Match` with the `ETag` you read, or the save answers 412 or 428.

## Runs keep their steps

When a case is added to a manual run (from a plan, or by hand), the run item **pins** the current version of
each shared step the case references. The runner shows the steps of that version, numbered as one tree, so a
shared step is step `2` and its steps are `2.1`, `2.2` and so on. The shared step itself takes a result like any
other step; with sub-steps its status follows them.

An edit made after the run started does not change that run. The runner shows `SS-12 v4` and, when a newer
version exists, `v5 available`.

- A run item that has **not started** (status `untested`, no step results) can take the new version: press
  **Update to current version**, or `POST /run-items/{itemId}/refresh`. An item with results answers 409
  `run_item_started`.
- **Rerunning** an item pins it again, so the new execution uses the versions that are current then. The
  earlier execution keeps the versions it ran.

Under [version review](workflows-and-review.md), a case and its shared steps run their **approved** version
while a newer one waits for review.

## Create, extract and archive

Open **Shared steps** in the project navigation. **New shared step** takes a name, a description and steps.
Steps have no attachments, and Markdown images cannot point at attachments, because a shared step has no
attachments of its own.

To make one from steps you already have, open the case, choose **Extract to shared step**, tick the steps and
give the new shared step a name. The steps must be **adjacent top-level steps**. They move into the shared
step and the case gets a reference in their place, as one new version of the case; nothing else changes.

```sh
curl -fsS -X POST "$api/cases/42/extract-shared" -H "$auth" -H 'If-Match: "r7"' \
  -H 'Content-Type: application/json' \
  -d '{"stepIds": ["s_0193a1", "s_0193a2"], "name": "Log in as a customer"}'
```

**Archive** hides a shared step from the picker and refuses **new** references (409 `shared_step_archived`);
cases that already use it keep working. Restore it from the archive list. **Delete for good** needs
`settings:write` and answers 409 `shared_step_in_use` with the number of cases and open runs while any case
uses it or an open run pins it.

**Inline** replaces the references to a shared step with copies of its current steps in every case that uses
it, as new case versions (source `bulk`), so you can retire the shared step. Up to 1000 usages are done in the
request (`200 {"updated": N, "skipped": M}`); more run as a background job (`202`, a `shared_inline` job on the
Jobs page). Cases that cannot take the change, for example the steps of an automated case that code owns, are
counted in `skipped` and stay as they are.

## The usage list

**Used by** on the shared step page lists the cases whose current or approved version references it, and how
many open manual runs pin it. `GET /shared-steps/{n}/usages` returns the same.

## API

| Method and path | Purpose | Permission |
|---|---|---|
| `GET /shared-steps` (`q`, `archived`, `before`, `limit`) | List; `q` is an `SS-` number, a name or words of the steps | `cases:read` |
| `POST /shared-steps` | Create | `cases:write` |
| `GET`, `PUT /shared-steps/{n}` | Read with `ETag`; replace with `If-Match` | `cases:read`, `cases:write` |
| `DELETE /shared-steps/{n}` | Archive; `?purge=true` deletes | `cases:write`, `settings:write` |
| `POST /shared-steps/{n}/restore` | Take out of the archive | `cases:write` |
| `GET /shared-steps/{n}/versions`, `/versions/{v}` | History and one snapshot | `cases:read` |
| `POST /shared-steps/{n}/versions/{v}/restore` | Restore a version as a new one (`If-Match`) | `cases:write` |
| `GET /shared-steps/{n}/usages` | Cases that use it | `cases:read` |
| `POST /shared-steps/{n}/inline` | Replace references with copies | `cases:write` |
| `POST /cases/{n}/extract-shared` | Extract adjacent steps (`If-Match` of the case) | `cases:write` |
| `GET /cases/{n}/shared-step-changes` | Edits of the shared steps a case uses | `cases:read` |
| `POST /run-items/{itemId}/refresh` | Pin an unstarted item to the current versions | `runs:execute` |

All paths are under `/api/v1/projects/{key}`. A shared step number from another project is 404, and a token
without access to the project gets 403, as for every other route.

Events: `sharedStep.updated {number, version, revision}` and `sharedStep.archived {number}` on the live
stream. The audit log records `sharedStep.create`, `update`, `archive`, `restore`, `restoreVersion`, `purge`,
`inline` and `extract`, and `runItem.refresh`.

## Limits

| Limit | Default | Environment variable |
|---|---|---|
| Shared steps per project (archived ones count) | 5000 | `FASTPROOF_TM_MAX_SHARED_STEPS` |
| References to shared steps in one case | 50 | `FASTPROOF_TM_MAX_SHARED_REFS` |
| Steps in one shared step, all levels | 100 | `FASTPROOF_TM_MAX_SHARED_STEP_STEPS` |
| Bytes of the steps JSON of one shared step | 65536 | `FASTPROOF_TM_MAX_SHARED_STEP_BYTES` |

Going over a limit answers 409 `limit_reached` with the name of the limit.

## During an upgrade

Writing the first `shared` step into a case is refused with 409 `upgrade_in_progress` while a server that does
not know shared steps is still running, for example during a rolling update. Create the shared steps first (that
is always allowed) and reference them once the upgrade is finished. See [Upgrading](../operations/upgrading.md).
