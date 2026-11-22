# Migrating from Allure Report

Allure Report builds a static HTML report from `allure-results` on every run, and keeps history by copying the
`history` directory between runs. FastProof is a server: CI uploads the same `allure-results`, and history,
flaky detection and triage live in one place for every branch and every job.

## What stays the same

- **Your tests and adapters.** Keep `allure-pytest`, `allure-junit5`, `allure-playwright`, `allure-js-commons`
  and the other adapters, their annotations and labels (`epic`, `feature`, `story`, `suite`, `owner`,
  `severity`, `tag`, `ALLURE_ID`), steps, parameters, links and attachments.
- **Test identity.** FastProof computes the same `testCaseId`/`fullName` hashes and parameter hashes as Allure
  2.45+ and Allure 3, so a test keeps its identity.
- **Categories.** `categories.json` is read with the results.
- **Environment.** `environment.properties` and `executor.json` become launch metadata.

## What changes

| Allure Report | FastProof |
|---|---|
| `allure generate` / `allure serve` in CI | `fastproof upload allure-results` (or `fastproof watch`) |
| Copy `allure-report/history` into the next run | Not needed: history is kept per series on the server |
| One report per job; shards merged by copying directories | One launch per pipeline; every shard uploads into it |
| Report published as static files (Pages, S3, artefacts) | One URL per launch, with access control per project |
| Retries shown per report | Retries become attempts of one result; flaky in run and across launches |
| Suites, Behaviors and Packages trees, `groupBy` | The launch opens on a tree grouped by suites, behaviors (epic › feature › story), packages, owner, severity or up to five labels of your choice, down to the tests; the selected test opens beside it, and its link opens the same view (see [Launch explorer](launch-explorer.md)) |
| Tree filters (status, flaky, retried, new, severity, tags) | The same, plus owner, muted, defect type, attachment kind, duration and error text, each value with its count, applied to the tree and the result list |
| `environments` matchers and the environment picker | Results carry the environment key of their series; a launch with several shows an environment switcher and per-environment counts |
| Known issues in `categories.json` or Allure 3 `resolutions` | Triage in the UI: defect types, issue links, mutes with expiry, applied by fingerprint |

## Steps

1. Run FastProof ([Getting started](getting-started.md) or the [Helm chart](../operations/helm.md)).
2. Create a project per product or team and a token with the `uploader` role for CI.
3. In the CI job, keep producing `allure-results` and replace `allure generate` with:

    ```sh
    fastproof upload allure-results
    ```

    with `FASTPROOF_URL`, `FASTPROOF_TOKEN` and `FASTPROOF_PROJECT` set, see [CI recipes](ci-recipes.md).

4. For sharded jobs, set `FASTPROOF_LAUNCH` to the pipeline id so all shards report into one launch.
5. Remove the steps that copied the `history` directory between runs.

You can keep generating the Allure HTML report next to FastProof while the team gets used to it; both read the
same `allure-results`.

## History import

Importing existing Allure history (the `history` directory of Allure 2 reports and the Allure 3 `historyPath`
JSONL file) comes in FastProof 1.1, right after the first release. Until then, history starts with the first
upload. Because identities match, imported history will join the series that FastProof already has.
