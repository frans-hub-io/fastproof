# Roadmap

FastProof grows in stages. The first release is the reporting server described on this site. What comes next,
in order of the stages, without dates:

## Right after the first release

- **Allure history import:** bring the history of existing Allure 2 reports and Allure 3 history files into
  FastProof, joined to the same test identities.
- **Launch tools:** debug launches, merging launches, reruns, and HTML and PDF export of a launch.
- **More own reporters:** the [Playwright, pytest, Vitest, Jest, Cypress, JUnit 5 and TestNG reporters](guide/reporters.md) come
  first, with live results and logs, a `traceparent` on every attempt, correct retries, and an offline spool.
  Reporters for more frameworks follow, chosen by how much today's writers lose.

## Test management

**Since 0.2.0-dev1:** [test cases](guide/test-management.md) next to the results they produce, manual and
automated tests in one case kept in sync with the code, [test plans and manual runs](guide/manual-runs.md)
combined with CI in one launch, and [imports](guide/importing-test-cases.md) from TestRail, Qase and Allure
TestOps (CSV formats built from the vendors' documented columns; the TestOps API import is a beta). Quick issue
links from manual runs open a prefilled form in the tracker; FastProof does not create the issue itself yet.

After that:

- shared steps with versions, parameters, configurable workflows and reviews of cases;
- requirements and the coverage of requirements by tests, a query language for filters and plans, and
  starting CI pipelines from a plan;
- creating issues in Jira, GitLab, GitHub and Azure
  DevOps directly from triage, with their status kept in sync.

## ReportPortal compatibility

Accept the ReportPortal agent protocol, so the existing ReportPortal agents of about twenty frameworks report
to FastProof without changes, and import data from ReportPortal.

## Available now for large installations

- **Kafka ingest:** uploads and live logs through Kafka, with the API, parsers, writers and sinks as separate
  Deployments.
- **ClickHouse sink:** results, attempts, steps and logs copied to ClickHouse for log search, similar
  failures and long-term aggregates. **OpenSearch** is supported as a sink and search backend too.

See [Scaling and big data](operations/scaling.md).

## Available now for triage and integrations

- [Rules and triage](guide/rules-and-triage.md): failure clusters, defect types, mutes, and rules with dry runs
  and retroactive application.
- [Quality gates](guide/quality-gates.md) and [notifications](guide/notifications.md): signed webhooks, Slack, Microsoft Teams,
  email, Telegram, merge request comments and commit statuses.
- [MCP for GitHub Copilot](guide/mcp-copilot.md): local agents read launches, failures, clusters, history,
  logs and FastMock interactions, and write triage that is marked as AI, audited and revertible, with
  ready-made `.vscode/mcp.json`, agent and prompt files.

## FastProof Analysis

An optional bundle, switched on in the Helm chart, for deeper analysis on top of the sinks; the core stays
light without it: failure clustering and "similar failures" across projects over the ClickHouse or OpenSearch
store. The MCP endpoint (`POST /mcp`) is not part of it: it ships with the server today, see
[MCP for GitHub Copilot](guide/mcp-copilot.md).

## Always

Stable releases with documented upgrade notes, a clear support policy for versions, and security fixes first.
