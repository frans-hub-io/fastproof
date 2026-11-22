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

A `viewer` token is enough to investigate; give an `editor` token only to agents that should triage. Every
tool checks the permission in the project's scope, exactly like the API. Writes through MCP are recorded with
`source: ai`, so people can tell them apart and change them, are never replaced by rules, and are written to
the audit log with `via: mcp`. Tool answers are bounded (256 KiB per answer, 64 KiB per log field).
Tools never return project secrets or notification targets; `fastmock_interactions` queries the project's
FastMock with its token only when the token's secret is bound to the origin of `fastmock.url` (see
[Project secrets](notifications.md#project-secrets)).

### Test case tools

<!-- TODO(T3): parameters, answers and limits of each tool as implemented; check the names and permissions. -->

The [test case](test-management.md) tools follow one rule: an agent writes only drafts and suggestions, and
people decide.

| Tool | Does | Needs |
|---|---|---|
| `search_cases` | Cases matching the filters of the case list, at most 100 | `cases:read` |
| `get_case` | A case with its steps (Markdown), fields, code identities and latest results | `cases:read` |
| `case_history` | Manual and automated results of a case | `cases:read` |
| `list_test_plans`, `get_test_plan` | Test plans and their cases | `cases:read` |
| `manual_run_progress` | Progress of the manual run of a launch, with failed steps and their actual results | `launches:read` |
| `trace_actions` | The actions of an attempt's Playwright trace (`apiName`, selector, URL, status, error, duration), at most 500 | `launches:read` |
| `draft_case_from_result` | A draft case built from a result: name, preconditions from fixtures, steps from the step tree, the error in the description | `cases:write` |
| `create_case_draft` | A draft case from the agent's text | `cases:write` |
| `propose_case_update` | A suggestion to change an existing case, against the revision the agent read; nothing is applied | `cases:write` |
| `add_cases_to_plan` | Add cases to a test plan | `plans:write` |

The server enforces the rules, whatever the agent asks for:

- cases an agent creates are always `draft`, with `origin = ai`, and marked **AI draft** in the UI until a
  person reviews them;
- an agent cannot activate a case, change an active case directly, accept a suggestion or execute manual
  steps; changes to existing cases arrive as suggestions on the case page, to accept or reject (a rejected
  one stays in the history);
- `trace_actions` never returns the values typed with `fill`, `type` or `press`, which can be passwords, nor
  masked test parameters;
- every write is in the audit log with `source: ai` and the agent's name.

## Good practice

- Use a personal token with the smallest role that fits, and set an expiry.
- Keep the token out of the repository: the `inputs` entry above makes VS Code prompt for it.
- Review what the agent proposes before it calls `triage`; the example agent is written to ask first.
- Rule drafts stay disabled until someone enables them on the **Rules** page, after a dry run.
