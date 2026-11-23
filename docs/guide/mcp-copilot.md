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
      "description": "FastProof API token (viewer to read, editor to triage)",
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
| `triage` | Set the defect type, comment, issue or mute of results or a whole cluster | `results:triage` |
| `create_rule_draft` | Create a disabled triage rule for people to review and enable; the audit log records it as AI-created (`"source": "ai"`) | `rules:write` |

`triage` and `create_rule_draft` take a defect type **id**, not a group name. Every project has its own ids
(the built-in types get them when the project is created) and no MCP tool lists them yet. Get them from
**Settings → Defect types** (the copy button next to a type), from `GET /api/v1/projects/{key}/defect-types`, or from the `defect.typeId` of results that
already have the type; the example prompt takes them as an input, and the example agent never guesses one.

A `viewer` token is enough to investigate; give an `editor` token only to agents that should triage. Every
tool checks the permission in the project's scope, exactly like the API. Writes through MCP are recorded with
`source: ai`, so people can tell them apart and change them, are never replaced by rules, and are written to
the audit log with `via: mcp`. Tool answers are bounded (256 KiB per answer, 64 KiB per log field).
Tools never return project secrets or notification targets; `fastmock_interactions` queries the project's
FastMock with its token only when the token's secret is bound to the origin of `fastmock.url` (see
[Project secrets](notifications.md#project-secrets)).

### Test case tools
The [test case](test-management.md) tools follow one rule: an agent writes only drafts and suggestions, and
people decide.

| Tool | Does | Needs |
|---|---|---|
| `search_cases` | Cases matching `q`, `folderId` (with subfolders), `statuses`, `automation`, `tags`, `fields`, `aiUnreviewed`; `limit` 1–100 (20); answers `{items, more}` | `cases:read` |
| `get_case` | A case by `number`: steps (Markdown), fields, tags, keys, code identities, `revision`, `key` (`TC-n`) and the 10 latest results | `cases:read` |
| `case_history` | Manual and automated results of a case (`branch`, `environment`, `source`, `limit` 1–100) | `cases:read` |
| `list_test_plans`, `get_test_plan` | Managed test plans (`q`, `limit` 1–100); one plan by `planId` with its cases (`limit` 1–500) | `cases:read` |
| `manual_run_progress` | The manual run of a launch (`launchId`): its counts and up to 50 failed or blocked items with their failed steps' actual results and comments | `launches:read` |
| `trace_actions` | The actions of the Playwright trace of an attempt (`resultId`, `attempt`): `apiName`, `selector`, `url`, `status`, `error`, `durationMs`, at most 500 (`truncated` beyond); traces up to 256 MiB | `launches:read` |
| `draft_case_from_result` | A draft case from a result (`resultId`, `attempt`): its name, preconditions from fixtures, steps from the step tree (3 levels), the failure message, a link to the result; the agent may give `name`, `notes`, `steps`, `folderId`, `tags` | `cases:write` |
| `create_case_draft` | A draft case from the agent's `name`, `description`, `preconditions`, `steps`, `folderId`, `tags`, `fields` | `cases:write` |
| `propose_case_update` | A suggestion for case `number` against `baseRevision` (`name`, `description`, `preconditions`, `steps`, `status`, `tags`, `fields`, a required `rationale`); validated like a case save, stored as pending, nothing applied; a moved revision answers `suggestion_stale` | `cases:write` |
| `add_cases_to_plan` | Add case `numbers` (1–1000) to a managed plan (`planId`) | `plans:write` |

Every write tool takes an optional `agent` (the agent's name, up to 100 bytes, recorded in the audit log).
Errors come back as tool errors whose text starts with the API error code (`forbidden`, `not_found`,
`invalid_case`, `suggestion_stale`, …).

The server enforces the rules, whatever the agent asks for, on the MCP tools and on the REST API alike:

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
  `caseSuggestion.create`, `testPlan.items`).

## Good practice

- Give the agent a dedicated token with the smallest role that fits, and set an expiry. Do not reuse the token
  you work with yourself: the rules above tell the agent and the reviewer apart by principal, so with a shared
  token you could not review the agent's drafts and suggestions with it. Review them signed in as yourself.
- Keep the token out of the repository: the `inputs` entry above makes VS Code prompt for it.
- Review what the agent proposes before it calls `triage`; the example agent is written to ask first.
- Rule drafts stay disabled until someone enables them on the **Rules** page, after a dry run.
