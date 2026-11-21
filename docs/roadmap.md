# Roadmap

FastProof grows in stages. The first release is the reporting server described on this site. What comes next,
in order of the stages, without dates:

## Right after the first release

- **Allure history import:** bring the history of existing Allure 2 reports and Allure 3 history files into
  FastProof, joined to the same test identities.
- **Launch tools:** debug launches, merging launches, reruns, and HTML and PDF export of a launch.
- **Own reporters (agents)** that write the FastProof JSON format directly: live streaming of results and
  logs, a `traceparent` on every attempt for FastMock correlation, correct retries where today's writers lose
  them, attachments uploaded straight to object storage, and an offline buffer when the server is not
  reachable.

## Test management

Test cases, suites and test plans next to the results they produce: manual and automated tests in one place,
test runs from plans, and coverage of requirements by tests. Also more dashboard widgets with cloning between
projects, and creating issues in Jira, GitLab, GitHub and Azure DevOps directly from triage, with their status
kept in sync.

## ReportPortal compatibility

Accept the ReportPortal agent protocol, so the existing ReportPortal agents of about twenty frameworks report
to FastProof without changes, and import data from ReportPortal.

## FastProof Analysis

An optional bundle, switched on in the Helm chart, for large installations and deeper analysis; the core stays
light without it:

- an analytical store fed from FastProof's event outbox, for full-text search over large volumes of logs,
  "similar failures" across projects, and long-term aggregates;
- an MCP endpoint (same tokens and permissions as the API) for local GitHub Copilot agents: read launches,
  failures, clusters, history, logs, attachments and FastMock interactions, and write triage (defect type,
  comment, issue link, mute, apply to a cluster), every AI change marked, audited and revertible;
- ready-made configurations in the documentation: `.vscode/mcp.json` and `.github` agent and prompt files for
  triaging a launch, explaining a failure and proposing a rule.

## Always

Stable releases with documented upgrade notes, a clear support policy for versions, and security fixes first.
