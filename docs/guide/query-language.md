# Query language

FPQL (FastProof query language) selects test cases with a line of text:

```text
status = active and automated = false and field["Priority"] in [high, critical]
  and folder in subtree("Payments/Cards") and not tag = legacy
  and (lastStatus in [failed, broken] or lastRunAt < now() - 14d)
```

The syntax follows AQL, the query language of Allure TestOps, where it can, so most TestOps filters move over
with small changes ([differences](#differences-from-aql)). The same query works in the case list, in bulk
actions, in exports, as the selection of a test plan, as a saved query and in the
[MCP tools](mcp-copilot.md).

The examples on this page use:

```sh
api=https://fastproof.example.com/api/v1/projects/web
auth="Authorization: Bearer $FASTPROOF_TOKEN"
```

## In the UI

Above the case list, switch the filters from **Filters** to **Query**. The chips you had set become the
equivalent query, so the switch is also a way to learn the syntax. The query bar completes attribute names,
operators, statuses, field names and field options as you type, shows how many cases match, and underlines
the part of a query that is wrong with the reason under the bar. Press Enter to apply. The query stays in the
page address, so a link to the list keeps it. The folder tree and the trash switch still apply on top of the
query.

The **Saved queries** menu next to the bar lists your own queries and the ones shared with the project.
Anybody who reads cases can save personal queries; sharing one, and changing or deleting a shared one, needs
`cases:write`.

In a managed test plan, **Edit filter** has the same **Filters | Query** switch: a plan can be kept in line with
a query instead of a filter, and **Sync** shows which cases the query now adds and which it no longer
matches, as it does for filters.

## Grammar

```text
query      := [condition-expression]
expression := term ("or" term)*
term       := factor ("and" factor)*
factor     := "not" factor | "(" expression ")" | condition
condition  := attribute operator value
            | attribute ("in" | "not in") "[" value ("," value)* "]"
            | attribute "is empty" | attribute "is not empty"
attribute  := name | field[key or "Name"]
operator   := = | != | ~ | !~ | > | >= | < | <=
```

- `not` binds tighter than `and`, and `and` tighter than `or`: `a or b and not c` is `a or (b and (not c))`.
  Use parentheses when in doubt.
- Keywords (`and or not in is empty true false`) and attribute names are not case-sensitive. Enum values
  (statuses, origins) are not case-sensitive either; names and tags are compared exactly, except with `~`.
- An empty query matches every case.

### Values

| Value | Examples |
|---|---|
| A word: letters, digits and `_ . : / -` | `active`, `smoke`, `TC-12`, `REQ-3`, `Payments/Cards` |
| A string in double or single quotes; `\"` and `\\` inside | `"Regression core"`, `'it\'s'` |
| A number | `12`, `-5`, `2.5` |
| `true`, `false` | `automated = true` |
| A date, `YYYY-MM-DD` (a whole UTC day) | `createdAt >= 2026-01-01` |
| `now()`, optionally shifted by days, hours or minutes | `lastRunAt < now() - 14d`, `updatedAt > now() - 12h` |
| `me()`: you | `createdBy = me()` |
| `subtree(path or id)`: a folder and its subfolders | `folder in subtree("Payments")` |

A date means its whole day: `createdAt = 2026-01-31` matches the whole day, `> 2026-01-31` starts on February 1,
`<= 2026-01-31` includes the 31st. `now()` takes only `<`, `<=`, `>` and `>=`.

## Attributes of cases

| Attribute | Operators | Values |
|---|---|---|
| `number` | `= != > >= < <= in` | `12` or `TC-12` |
| `name` | `= != ~ !~ in` | text; `~` matches a part of the name |
| `text` | `~ !~` | words of the name, description, preconditions and steps (full-text search) |
| `status` | `= != in` | `draft`, `active`, `outdated` |
| `state` | `= != in`, `is empty` | a [workflow](workflows-and-review.md) state key; empty without a workflow |
| `automated` | `= !=` | `true`, `false` |
| `folder` | `= != in`, `is empty` | a folder path (`"Payments/Cards"`, names not case-sensitive) or id, or `subtree(...)` |
| `tag` | `= != ~ !~ in`, `is empty` | a tag |
| `field[key]`, `field["Name"]` | `= != in`, `is empty`; `~ !~` for text and select fields; `> >= < <=` for number and date fields | a custom field by key or name |
| `createdBy`, `updatedBy` | `= != ~ !~ in`, `is empty` | a user name or `me()` |
| `createdAt`, `updatedAt` | `= != > >= < <=` | a date or `now() - …` |
| `origin` | `= != in` | `ui`, `code`, `import`, `ai` |
| `aiUnreviewed` | `= !=` | an AI draft nobody acknowledged yet |
| `review` | `= != in`, `is empty` | `pending`, `changes_requested`, `approved`; empty when review does not apply |
| `lastStatus` | `= != in`, `is empty` | status of the newest result: `passed`, `failed`, `broken`, `skipped`, `unknown`; empty when never run |
| `lastRunAt` | `= != > >= < <=`, `is empty` | time of the newest result |
| `lastSource` | `= != in`, `is empty` | `manual`, `automated` |
| `requirement` | `= != in`, `is empty` | a [requirement](requirements.md) the case covers: `REQ-12` |
| `sharedStep` | `= != in`, `is empty` | a [shared step](shared-steps.md) the case uses: `SS-3` |
| `plan` | `= != in` | a managed test plan by id or name |
| `hasParameters` | `= !=` | the case has [parameters](parameters.md) |
| `staleDays` | `> >= < <=` | days an automated case has had no result from a trusted branch; never is stale |
| `archived` | `= !=` | archived cases (the trash); see below |
| `filter` | `= !=` | a saved query by name: `filter = "Regression core"` |

`!=`, `not in` and `not` match exactly the cases the positive form does not, including cases with no value:
`createdBy != alice` includes cases nobody is recorded as creating, and `field[priority] != high` includes cases
without a priority. Use `is empty` / `is not empty` to ask about missing values; a field is empty when the case
has no value for it.

Lists, bulk actions and validation leave archived cases out unless the query names `archived` (for example
`archived = true and tag = legacy`), or the list is asked for the trash. Exports and plans never include
archived cases.

### Saved queries inside queries

`filter = "Name"` inserts a saved query: your own query of that name, else a shared one. Saved queries may
name other saved queries up to 4 levels deep; a query that ends up naming itself is refused with the chain
of names in the error. A query with its saved queries inserted has at most 256 conditions.

## Where queries work

| Where | How |
|---|---|
| Case list | `GET $api/cases?query=...`, together with `folder`, `recursive`, `archived`, `sort`, `before`, `limit` and the structured filters (all ANDed) |
| Bulk actions | `POST $api/cases/bulk` with `query` instead of `numbers` or `filter` |
| Exports | `GET $api/cases/export?format=csv&query=...`; the zip job `POST $api/cases/exports {"query": ...}` |
| Test plans | `query` on `POST $api/test-plans` (`kind: managed`) and `PATCH $api/test-plans/{id}`, instead of `filter`; `POST $api/test-plans/{id}/sync` shows the diff |
| Saved queries | `GET`, `POST $api/saved-queries`, `PATCH`, `DELETE $api/saved-queries/{id}` |

```sh
curl -sG -H "$auth" "$api/cases" --data-urlencode 'query=tag = smoke and lastStatus in [failed, broken]'

curl -s -H "$auth" -H 'Content-Type: application/json' -X POST "$api/cases/bulk" \
  -d '{"query": "automated = true and lastRunAt < now() - 90d", "action": "setStatus", "value": "outdated"}'

curl -s -H "$auth" -H 'Content-Type: application/json' -X POST "$api/test-plans" \
  -d '{"kind": "managed", "name": "Payments regression", "query": "folder in subtree(\"Payments\") and status = active"}'
```

A plan keeps either a filter or a query: sending one with the other `null` switches the plan, sending both is
400. `me()` in a plan query means whoever syncs the plan.

### Checking a query

`POST $api/query/validate` checks a query without listing cases. It answers 200 in both cases: with the
normalized text and the number of matching cases (up to 10 000; `countCapped` says there are more), or with
`ok: false` and the error.

```sh
curl -s -H "$auth" -H 'Content-Type: application/json' -X POST "$api/query/validate" \
  -d '{"entity": "cases", "query": "status = actve"}'
```

```json
{"ok": false, "error": {"position": 9, "length": 5, "expected": ["draft", "active", "outdated"],
 "near": "actve", "message": "status takes one of: draft, active, outdated"}}
```

`position` and `length` count UTF-16 code units from 0, the way browsers and editors count characters. Every
other route answers an invalid query with 400 `invalid_query` and the same object in `details`.

`GET $api/query/schema?entity=cases` lists the attributes with their operators and values, the functions, the
project's fields (with the options of select fields) and the keywords: what the query bar completes from.

### Saved queries

```sh
curl -s -H "$auth" -H 'Content-Type: application/json' -X POST "$api/saved-queries" \
  -d '{"entity": "cases", "name": "Flaky smoke", "query": "tag = smoke and lastStatus = broken", "shared": true}'
```

| Rule | |
|---|---|
| Personal queries | anybody with `cases:read`; only their owner sees, changes or deletes them (others get 404) |
| Shared queries | created, changed and deleted with `cases:write`; everybody who reads cases sees and uses them; only the owner shares or unshares |
| Names | 1 to 200 bytes, unique per owner and entity (409 `saved_query_name_taken`) |
| Limits | 100 per owner and entity, 200 shared per project and entity (`FASTPROOF_TM_MAX_SAVED_QUERIES` sets the first; the second is twice it) |

A saved query is checked when it is saved, like a query that runs. The audit log records `savedQuery.create`,
`savedQuery.update` and `savedQuery.delete`; the text of personal queries is left out.

## Limits

| Limit | Value |
|---|---|
| Length of a query | 4096 bytes |
| Conditions | 64 (256 with saved queries inserted) |
| Nesting of parentheses and `not` | 8 levels |
| `~` | at least 3 characters |
| Values in one `in [...]` list | 1000 |
| Running time | every statement that runs a query stops after 5 s: 400 `invalid_query` "the query ran longer than 5s" |
| Count of `validate` | 10 000 |

Bulk actions, exports and validation also wait for a slot of the per-caller limit of heavy queries. Requirement
queries (`entity=requirements`) arrive with [requirements](requirements.md); until then they answer 400.

## Differences from AQL

| AQL (Allure TestOps) | FPQL |
|---|---|
| `status = "Active"` (workflow names) | `status` is the category (`draft`, `active`, `outdated`); workflow states are `state` |
| `cf["Priority"] = "High"` | `field["Priority"] = high` (by name or key) |
| `layer`, `tag`, `member[...]` | `field[layer]`, `tag`, `createdBy`/`updatedBy` |
| `createdDate > -7d` | `createdAt > now() - 7d`; absolute dates are `YYYY-MM-DD` |
| `issue = "JIRA-1"` | `requirement = REQ-12` (requirements are their own entity) |
| `launch`, `result` and defect attributes | not available: FPQL selects cases, not results |
| `!=` skips missing values | `!=` and `not` include cases without a value; use `is empty` |

Queries never become SQL text: every value is passed to PostgreSQL as a parameter, so quotes, comments or
`$1` inside a value only ever compare as text.
