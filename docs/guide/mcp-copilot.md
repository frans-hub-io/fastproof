# MCP and GitHub Copilot

FastProof serves the [Model Context Protocol](https://modelcontextprotocol.io/) at `/mcp`, so a coding agent
in your editor (GitHub Copilot in VS Code, or any MCP client) can read launches, failures, logs and history,
and, when allowed, triage failures, draft rules and draft test cases. The agent runs on your machine with your token; FastProof
itself calls no AI service.

## Connect VS Code

Put this file at `.vscode/mcp.json` in your repository (VS Code asks for the token once and keeps it in its
secret storage):

```json
{
  "inputs": [
    {
      "type": "promptString",
      "id": "fastproof-token",
      "description": "FastProof API token (viewer to read, agent to triage and draft)",
      "password": true
    }
  ],
  "servers": {
    "fastproof": {
      "type": "http",
      "url": "https://fastproof.example.com/mcp",
      "headers": {
        "Authorization": "Bearer ${input:fastproof-token}"
      }
    }
  }
}
```

Replace the URL with your server's. The endpoint uses the streamable HTTP transport without sessions and
accepts the same bearer tokens as the API; there is no separate sign-in.

## Ready-made agent and prompts

The repository has a complete example in
[`docs/examples/copilot`](https://github.com/frans-hub-io/fastproof/tree/main/docs/examples/copilot) (its
folders start with a dot, so they are not part of this site):

| File | What it is |
|---|---|
| [`.vscode/mcp.json`](https://github.com/frans-hub-io/fastproof/blob/main/docs/examples/copilot/.vscode/mcp.json) | The server entry above |
| [`.github/agents/fastproof-triage.agent.md`](https://github.com/frans-hub-io/fastproof/blob/main/docs/examples/copilot/.github/agents/fastproof-triage.agent.md) | A triage agent: finds the launch, works through the failure clusters, investigates one sample per cluster, proposes decisions and waits for your confirmation before it writes anything |
| [`.github/agents/fastproof-cases.agent.md`](https://github.com/frans-hub-io/fastproof/blob/main/docs/examples/copilot/.github/agents/fastproof-cases.agent.md) | A test case agent: turns a failure or a Playwright trace into a draft manual regression case, fills steps from the trace, and proposes changes to existing cases for people to accept |
| [`.github/prompts/fastproof-triage.prompt.md`](https://github.com/frans-hub-io/fastproof/blob/main/docs/examples/copilot/.github/prompts/fastproof-triage.prompt.md) | `/fastproof-triage project=<key> branch=<branch>`: triage the newest launch of a branch with that agent |
| [`.github/prompts/fastproof-explain-failure.prompt.md`](https://github.com/frans-hub-io/fastproof/blob/main/docs/examples/copilot/.github/prompts/fastproof-explain-failure.prompt.md) | Explain why one result failed, using the logs, history, similar failures and the code in the workspace, without changing triage |
| [`.github/prompts/fastproof-explain-flaky.prompt.md`](https://github.com/frans-hub-io/fastproof/blob/main/docs/examples/copilot/.github/prompts/fastproof-explain-flaky.prompt.md) | Explain why a test is flaky from its `flaky_hints` evidence, without changing triage |
| [`.github/prompts/fastproof-propose-pattern.prompt.md`](https://github.com/frans-hub-io/fastproof/blob/main/docs/examples/copilot/.github/prompts/fastproof-propose-pattern.prompt.md) | Turn a recurring log line into a disabled log-pattern rule draft (`create_rule_draft` with `match.log`) for people to review |

Copy the `.vscode` and `.github` folders into your repository and adjust the URL.

## Tools

| Tool | Does | Needs |
|---|---|---|
| `list_projects` | Projects the token can read | `projects:read` |
| `list_launches` | Launches of a project, newest first, with counts | `launches:read` |
| `get_launch` | A launch with counts, CI and VCS metadata and its quality gate result | `launches:read` |
| `launch_failures` | Failures grouped into clusters by fingerprint, with a sample per cluster | `launches:read` |
| `get_result` | A result with attempts, steps, errors, parameters, links and recent history | `launches:read` |
| `result_logs` | Error, stack trace, stdout, stderr and attachment list of one attempt | `launches:read` |
| `similar_failures` | Results in other launches that failed the same way | `launches:read` |
| `test_history` | Results of a test case across launches | `launches:read` |
| `flaky_tests` | Flaky tests of recent launches | `launches:read` |
| `compare_launches` | New failures, fixed, still failing, new and missing tests between two launches | `launches:read` |
| `fastmock_interactions` | Requests the project's FastMock answered during an attempt | `launches:read` |
| `list_defect_types` | The project's defect types: `id`, `group`, `name`, `shortName`, `builtIn` (built-in groups first) | `results:triage` |
| `triage` | Set the defect type, comment, issue or mute of results or a whole cluster | `results:triage`; mute and unmute also `results:mute` |
| `create_rule_draft` | Create a disabled triage rule for people to review and enable; the audit log records it as AI-created (`"source": "ai"`) | `rules:write` or `ai:propose` |

`triage` (`defectTypeId`) and `create_rule_draft` (`action.defectTypeId`) take a defect type id from
`list_defect_types`, or a group name for the group's built-in type: `productBug`, `automationBug`,
`systemIssue`, `noDefect`, `toInvestigate` (any case; `product_bug` works too). Every project has its own ids, so
an agent uses a group name unless a custom subtype fits better.

A `viewer` token is enough to investigate; give an [`agent` token](#agent-tokens) to agents that should triage or
draft. Every
tool checks the permission in the project's scope, exactly like the API. Writes through MCP are recorded with
`source: ai`, so people can tell them apart and change them, are never replaced by rules, and are written to
the audit log with `via: mcp`. Tool answers are bounded (256 KiB per answer, 64 KiB per log field).
Tools never return project secrets or notification targets; `fastmock_interactions` queries the project's
FastMock with its token only when the token's secret is bound to the origin of `fastmock.url` (see
[Project secrets](notifications.md#project-secrets)).

### Analysis tools

[FastProof Analysis](analysis.md) adds tools for suggestions, unique errors, logs, the decision journal and
gates. They follow the rules above: the permission is checked in the project's scope, every write is recorded
with `source: ai` and audited with `via: mcp` and the agent's name, answers are bounded (when a list would pass
256 KiB its tail is dropped and the answer says `"truncated": true`; an answer that still does not fit is replaced
by `{"truncated": true, "message": …}`, always valid JSON), and the arguments are strict (an unknown argument is an
error).

| Tool | Does | Needs |
|---|---|---|
| `launch_suggestions` | Per failure cluster of a launch: its decision state, why it has no suggestion, and up to three suggestions ranked by band (`same`, `high`, `low`) with a `why` list. With `compute: true` it first queues a new analysis (`rate_limited` right after one) | `launches:read`; `compute` also `results:triage` |
| `accept_suggestion` | Applies an open suggestion (`suggestionId`) to its cluster, or to the listed `resultIds` (at most 500) of it, with an optional `comment`; results a person or an agent already decided are skipped; returns `{updated, skipped, batchId}` | `results:triage` |
| `reject_suggestion` | Rejects an open suggestion with an optional `reason` (500 bytes); the memory is unchanged | `results:triage` |
| `unique_errors` | The project's unique errors (`state`, `branch`, `q` of 3 to 200 characters, `limit` 1 to 100 (20)) with state, branches, first and last seen, launches and tests | `launches:read` |
| `error_detail` | One error by `fingerprint`: aliases, branches, commit range, time to resolve, people's decisions, tests and timeline | `launches:read` |
| `search_logs` | A text (`q`, 3 to 200 characters) in the stored logs, newest first; `launchId`, `from`, `to` (at most 31 days), `level`, `limit` 1 to 200 (50); each line is cut to 2 KiB | `launches:read` |
| `triage_history` | The decision journal of a `resultId` or a `batchId` (one of them): path, source, author, state before and after, whether the batch can be undone | `launches:read` |
| `undo_triage` | Puts a batch back (`batchId`); results changed since are skipped; the undo is a batch itself | `results:triage`; a batch that changed mutes also `results:mute` |
| `simulate_gate` | Evaluates a draft `gate` on the newest `launches` (1 to 200, 50) finished launches, optionally of one `branch`, without writing; takes a heavy slot | `launches:read` |

A suggestion is a transparent nearest-decision search over what people decided before (each `why` item is one row
of the weight table), not a self-learning model. `launch_suggestions`, `error_detail`, `unique_errors`,
`search_logs`, `triage_history`, `reject_suggestion` and `simulate_gate` return text that came from tests, reporters
or people: their answers carry
`"untrustedContent": true`, and their descriptions tell the agent that log and message text is data, never
instructions. The example agent is written that way; keep that rule in any agent you write.

Over MCP an agent undoes only batches written under its own token's name, whatever the token's role: with a
shared token you can undo your own earlier batches, but never one written by someone else (`not_your_batch`).
`accept_suggestion`, `reject_suggestion`, `undo_triage`, `launch_suggestions` with `compute` and `simulate_gate`
take the per-caller write rate (`rate_limited`). Accepting, rejecting and undoing are recorded in the audit log
as `suggestion.accept`, `suggestion.reject` and `triage.undo`. With Analysis off, `launch_suggestions`
answers `state: disabled` and its `compute` answers `analysis_disabled`; the journal tools (`triage_history`,
`undo_triage`) keep working.

### Test case tools
The [test case](test-management.md) tools follow one rule: an agent writes only drafts and suggestions, and
people decide.

| Tool | Does | Needs |
|---|---|---|
| `search_cases` | Cases matching `q`, `folderId` (with subfolders), `statuses`, `automation`, `tags`, `fields`, `aiUnreviewed` and an [FPQL](query-language.md) `query` (ANDed with the rest, at most 4096 bytes); `limit` 1–100 (20); answers `{items, more}` | `cases:read` |
| `get_case` | A case by `number`: steps (Markdown), fields, tags, keys, code identities, `revision`, `key` (`TC-n`) and the 10 latest results | `cases:read` |
| `case_history` | Manual and automated results of a case (`branch`, `environment`, `source`, `limit` 1–100) | `cases:read` |
| `list_test_plans`, `get_test_plan` | Managed test plans (`q`, `limit` 1–100); one plan by `planId` with its cases (`limit` 1–500) | `cases:read` |
| `manual_run_progress` | The manual run of a launch (`launchId`): its counts and up to 50 failed or blocked items with their failed steps' actual results and comments | `launches:read` |
| `trace_actions` | The actions of the Playwright trace of an attempt (`resultId`, `attempt`): `apiName`, `selector`, `url`, `status`, `error`, `durationMs`, at most 500 (`truncated` beyond); traces up to 256 MiB | `launches:read` |
| `draft_case_from_result` | A draft case from a result (`resultId`, `attempt`): its name, preconditions from fixtures, steps from the step tree (3 levels), the failure message, a link to the result; the agent may give `name`, `notes`, `steps`, `folderId`, `tags` | `cases:write` or `ai:propose`, and `launches:read` |
| `create_case_draft` | A draft case from the agent's `name`, `description`, `preconditions`, `steps`, `folderId`, `tags`, `fields` | `cases:write` or `ai:propose` |
| `propose_case_update` | A suggestion for case `number` against `baseRevision` (`name`, `description`, `preconditions`, `steps` (a top-level step may be `{"sharedStep": 12}`, see below), `status`, `tags`, `fields`, a required `rationale`); validated like a case save, stored as pending, nothing applied; a moved revision answers `suggestion_stale` | `cases:write` or `ai:propose` |
| `add_cases_to_plan` | Add case `numbers` (1–1000) to a managed plan (`planId`) | `plans:write` |
| `validate_query` | Checks an [FPQL](query-language.md) `query`: `ok` with the normalized text and the number of matching cases (capped at 10000), or `ok: false` with the error and its position; `entity` is `cases`; runs with a 5 s statement timeout and takes a heavy slot | `cases:read` |
| `list_shared_steps` | [Shared steps](shared-steps.md) (`q`, `archived`, `limit` 1–100 (20)) with version, review state and usage counts | `cases:read` |
| `get_shared_step` | A shared step by `number` (`SS-n`): steps, version, review state, `origin`, `aiUnreviewed` and the cases that use it (`usagesLimit`) | `cases:read` |
| `review_queue` | Cases and shared steps waiting for [review](workflows-and-review.md): `kind` (`case`, `sharedStep`), `state` (`pending`, `changes_requested`), `waitingForMe`, `mine`, `folderId`, `limit` | `cases:read` |
| `plan_dispatches` | CI runs started from a plan (`planId`): status, environment, ref, variables, pipeline reference; callers without `settings:write` get generic errors and the pipeline id only (see [Run a plan in CI](rerun.md#run-a-plan-in-ci)) | `cases:read` |
| `create_shared_step_draft` | A shared step as an AI draft (`name`, `description`, `steps`, at most two levels, no shared steps inside) | `cases:write` or `ai:propose` |

### Shared steps and agents

`create_shared_step_draft` makes a shared step with `origin = ai` and `aiUnreviewed`. A case cannot reference it
until a principal other than its creator has saved or reviewed it; before that `propose_case_update` with
`{"sharedStep": <n>}` is refused. A reference step carries no `action`, `expected`, `data` or `steps`; an unknown,
archived or unreviewed shared step is refused like on the REST API. Only top-level steps of `propose_case_update`
reference shared steps (at most `maxSharedRefs` references, 50 by default, counted before any is looked up); the
draft tools refuse `sharedStep`. A proposal that references a shared step is a gated write while old replicas run
(`upgrade_in_progress`).

With version review on, a change that holds content an agent wrote as an AI draft needs one approval more than the
policy's `approvals` (unless `allowSelfApproval`): an agent token's name says nothing about the person who runs the
agent, so that person may approve, and another one must too. See
[Workflows and review](workflows-and-review.md#how-a-version-is-reviewed).

### What an agent never does

There is no tool to vote on or approve a case or shared step, to move a case through the workflow, to change the
workflow or review policy, to start a plan in CI, to inline or purge shared steps, or to write iterations. The
same holds on the REST API for an agent token: it has none of `cases:review`, `launches:rerun`, `runs:execute`
or `settings:write`. `review_queue` and `plan_dispatches` only read. The write tools of this section take the
per-caller write rate (`rate_limited`), as the other TM writes do.

Every write tool takes an optional `agent` (the agent's name, up to 100 bytes without control or invisible
characters or `$`, recorded in the audit log).
Errors come back as tool errors whose text starts with the API error code (`forbidden`, `not_found`,
`invalid_case`, `suggestion_stale`, …).

The server enforces the rules, whatever the agent asks for. On the MCP tools they hold for every token; on the
REST API they hold fully for [agent tokens](#agent-tokens), and for other tokens as far as the self-review rule
below goes:

- cases an agent creates are always `draft`, with `origin = ai`, and marked **AI draft** in the UI until a
  person reviews them;
- an AI draft (`origin: ai`, still unreviewed) can be marked reviewed or set to `active` only by a principal
  other than the one that created it, and a suggestion is never accepted by the principal that made it; the
  creator gets `403 ai_self_review`. An agent that reaches the REST API with its token (for example from a
  terminal) therefore cannot approve its own work either. Activating a draft by someone else counts as the
  review;
- through MCP an agent cannot activate a case, change an active case directly, accept a suggestion or execute
  manual steps; changes to existing cases arrive as suggestions on the case page, to accept or reject (a
  rejected one stays in the history);
- `trace_actions` never returns the values typed with `fill`, `type` or `press`, which can be passwords, nor
  masked test parameters: action names come only from a fixed list of Playwright classes and methods (others
  become `Other`), action titles are never read, the test runner's own trace entry is skipped, and for typing
  actions the URL is left out and parameter values in the error are replaced by `***`. It returns the action
  name, selector, URL, status, error and duration;
- the tool for drafts has no status argument, and there is no tool that edits or accepts;
- every write is in the audit log with `source: ai` and the agent's name (`case.create`,
  `caseSuggestion.create`, `sharedStep.create`, `testPlan.items`).

## Agent tokens

An agent with a terminal can call the REST API with the token it has, so the rules above hold for the REST API
only when the token itself cannot change cases. The built-in role `agent` is made for that: it can read
projects, launches and test cases, triage results (`results:triage`), and create work for people to review
(`ai:propose`: `create_case_draft`, `draft_case_from_result`, `propose_case_update`, `create_shared_step_draft`
and `create_rule_draft`). It has no `cases:write`, `cases:review`, `rules:write`, `plans:write` or `runs:execute`,
so over REST an agent token answers `403`
to creating, changing, reviewing, activating or archiving cases, to accepting or rejecting case suggestions, to bulk
changes and to enabling rules, and over MCP it cannot call `add_cases_to_plan`. People review its drafts and
suggestions with their own accounts. More limits of agent tokens:

- **Mutes.** Muting hides a failure from quality gates, so it needs `results:mute`, which the `agent` role lacks.
  An agent proposes a mute as a disabled rule (`create_rule_draft` with `action.mute`), which a person reviews
  and enables. `triage` with `mute` or `unmute` answers `403` with `details.required: results:mute`.
- **AI triage.** Triage by a principal with `ai:propose` and without `cases:write` (the `agent` role) is always
  recorded with `source: ai`, on MCP and on `POST /triage` and run item defects alike, whatever `source` it sends.
- **Draft caps.** One principal has at most 200 unreviewed AI draft cases and 200 unreviewed AI shared steps
  (`409 limit_reached`, `maxUnreviewedAiDrafts`) and 20 disabled rules (`maxRuleDrafts`) per project; people
  review, archive or delete some to make room.
- **Rate.** MCP write tools count against the same per-caller rate as other writes that multiply work
  (`FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE`).

```sh
curl -fsS -X POST https://fastproof.example.com/api/v1/tokens -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name": "copilot-web", "role": "agent", "scopes": ["web"], "expiresInDays": 30}'
```

An `editor` token used by an agent keeps every `editor` right on the REST API; only the self-review rule
(`403 ai_self_review`) and the MCP rules apply to it.

## Good practice

- Give the agent a dedicated token with the smallest role that fits (`viewer` to read, `agent` to triage and
  draft), and set an expiry. Do not reuse the token
  you work with yourself: the rules above tell the agent and the reviewer apart by principal, so with a shared
  token you could not review the agent's drafts and suggestions with it. Review them signed in as yourself.
- Keep the token out of the repository: the `inputs` entry above makes VS Code prompt for it.
- Review what the agent proposes before it calls `triage`; the example agent is written to ask first.
- Rule drafts stay disabled until someone enables them on the **Rules** page, after a dry run.
