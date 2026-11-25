# Rerun from a launch

**Rerun failed** and **Rerun selected** on a launch page make a test plan (Allure `testplan.json`) of the tests
to run again and trigger the CI that produced the launch. The CI is told which plan to run and which external
id its new launch must use; the new launch then shows **Rerun of #N** and the original lists it.

The CI must run only the tests of the plan. Allure adapters do this when `ALLURE_TESTPLAN_PATH` points at the
file (see [Reporters](reporters.md)).

## Settings

The CI is configured per project in **Settings** › **Integrations** › **Rerun in CI**, or in the `rerun` key of
`PUT /api/v1/projects/{key}/settings` (needs `settings:write`). Every change of the section is written to the
audit log (`project.settings.rerun`, old and new value; secret names, never values).

```json
{
  "rerun": {
    "provider": "github",
    "url": "https://api.github.com",
    "repository": "acme/shop",
    "workflow": "tests.yml",
    "ref": "main",
    "tokenSecretRef": "ci-token"
  }
}
```

| `provider` | What happens | Settings |
|---|---|---|
| `github` | `POST /repos/{repository}/actions/workflows/{workflow}/dispatches` (`workflow_dispatch`) with the token as a bearer token | `url` (`https://api.github.com`, or `https://host/api/v3` for GitHub Enterprise), `repository`, `workflow`, `ref` (default `main`), `tokenSecretRef` |
| `gitlab` | `POST /api/v4/projects/{repository}/trigger/pipeline` with a pipeline trigger token | `url` (the origin only), `repository`, `ref`, `tokenSecretRef` (the trigger token) |
| `webhook` | `POST` of a JSON document to `url`, signed (below); an optional `tokenSecretRef` is also sent as `Authorization: Bearer` | `url`, `signingSecretRef` (recommended), optional `tokenSecretRef` |

`url` takes no user info, query or fragment, and GitHub and GitLab take no path beyond the one shown. `ref` is
only ever taken from the settings, never from the launch, whose branch comes from whoever uploaded it.

### The CI secret is bound to its target

The token is a [project secret](notifications.md#project-secrets) with purpose `ci`, bound to the origin of `url`
and to a **scope** that names what it is for: `github:<owner/repo>:<workflow>`, `gitlab:<group/project>` or
`webhook:<URL>`. The settings must name exactly that target. Pointing the settings at another provider,
repository, workflow or webhook URL is refused when they are saved and again when the token would be used;
the secret has to be written again for the new target.

```json
PUT /api/v1/projects/shop/secrets/ci-token
{"value": "<token>", "purpose": "ci", "origin": "https://api.github.com", "scope": "github:acme/shop:tests.yml"}
```

Use a fine-grained GitHub token limited to that one repository with only *Actions: write*, or a GitLab pipeline
trigger token (it can start pipelines of one project and nothing else).

### What the CI receives

| | GitHub input | GitLab variable | Webhook field |
|---|---|---|---|
| the test plan | `fastproof_plan_id` | `FASTPROOF_PLAN_ID` | `planId`, `planUrl`, and `testplan` (up to 2000 tests) |
| external id of the new launch | `fastproof_launch` | `FASTPROOF_LAUNCH` | `newLaunchExternalId` |
| the original launch | `fastproof_source_launch` | `FASTPROOF_SOURCE_LAUNCH` | `launch` |
| FastProof URL (when `FASTPROOF_PUBLIC_URL` is set) | `fastproof_url` | `FASTPROOF_URL` | `planUrl` |

Treat all of it as untrusted data: test names and selectors in the plan, and the launch name, branch and commit
in a webhook body, come from whoever uploaded the results. Never put them into a shell command line.

A GitHub workflow must declare the inputs under `on.workflow_dispatch.inputs`, otherwise GitHub refuses the
dispatch. Anyone who can dispatch the workflow by hand can send any input, so the job passes the inputs through
`env:`, validates their format, and takes the FastProof URL from a repository variable, never from an input
(otherwise a dispatcher could send `FASTPROOF_TOKEN` to a server of their own):

```yaml
on:
  workflow_dispatch:
    inputs:
      fastproof_plan_id: {type: string}
      fastproof_launch: {type: string}
      fastproof_source_launch: {type: string}

jobs:
  rerun:
    runs-on: ubuntu-latest
    env:
      PLAN_ID: ${{ inputs.fastproof_plan_id }}
      LAUNCH: ${{ inputs.fastproof_launch }}
      FASTPROOF_URL: ${{ vars.FASTPROOF_URL }}
      FASTPROOF_TOKEN: ${{ secrets.FASTPROOF_TOKEN }}
    steps:
      - run: |
          [[ "$PLAN_ID" =~ ^[0-9a-f-]{36}$ ]] || exit 1
          [[ "$LAUNCH" =~ ^rerun-[0-9a-f-]{36}$ ]] || exit 1
          fastproof testplan --plan "$PLAN_ID" > testplan.json
          ALLURE_TESTPLAN_PATH=testplan.json FASTPROOF_LAUNCH="$LAUNCH" npm test
```

Joining a launch by external id is the way every reporter already joins a launch, so the link needs no extra
step in the CI.

### Webhook signature

With `signingSecretRef` (a `ci` secret with the same scope, `webhook:<the URL>`) every call carries
`X-FastProof-Timestamp` (Unix seconds), `X-FastProof-Delivery` (the rerun id), `X-FastProof-Event: rerun` and
`X-FastProof-Signature: sha256=<hex>`, the HMAC-SHA256 with the key over `<timestamp>.<raw body>`. A receiver
verifies it over the raw body, compares in constant time, rejects timestamps more than a few minutes off its
clock and drops a delivery id it has seen:

```python
import hmac, hashlib, time

def verify(key: bytes, timestamp: str, body: bytes, signature: str) -> bool:
    if abs(time.time() - int(timestamp)) > 300:
        return False
    mac = hmac.new(key, timestamp.encode() + b"." + body, hashlib.sha256).hexdigest()
    return hmac.compare_digest("sha256=" + mac, signature)
```

## API and CLI

`POST /api/v1/projects/{key}/launches/{id}/rerun` needs `launches:rerun`. The `editor` and `admin` roles hold
it; the `uploader` role (the CI report tokens, which often run code that is less trusted than the stored CI
token) does not, and neither do `viewer` and `tester`. Grant it to a token or group only where it should start
pipelines.

```json
{"mode": "failed", "statuses": ["failed", "broken"]}
{"mode": "selected", "resultIds": ["..."]}
```

It answers `201` with the rerun (`id`, `externalId`, `planId`, `testCount`, `status`, `ciRef`). `409` means the
project has no `rerun` settings or no test would run; `429` that the launch was rerun less than 10 seconds ago,
20 times already (failed attempts do not count), or the project had 60 reruns in the last hour (every attempt
counts; the limit is enforced under a project-wide lock, so parallel requests cannot go past it); `502` that
the CI refused or was unreachable (the rerun is kept with the reason).
`GET /api/v1/projects/{key}/launches/{id}/reruns` lists the reruns of a launch with the launches they started
and the rerun that started this launch (`launches:read`). Callers without `settings:write` get generic error
texts and only the pipeline id as `ciRef`: the CI host and secret names are settings details.

```sh
fastproof rerun --url https://fastproof.example.com --project shop --launch <id or external id>
fastproof rerun --launch <id> --result <result id> --result <result id>
```

## Run a plan in CI

A managed plan with automated cases can start the CI itself: **Start launch** on the plan page has a **Run
autotests in CI** switch (needs `runs:execute` and `launches:rerun`, and the `rerun` settings above). FastProof
starts one launch per environment you name (at most 10; none named: one launch), then triggers the CI once per
launch with the plan and the launch's external id, through the same provider, token and egress rules as a rerun.
The CI runs the plan's automated tests and uploads into the launch it was told to join; the manual cases of the
plan get their run items in the same launches unless you turn **manual cases** off.

A CI refusal does not undo the launches: the answer is `201` and each dispatch has its own `status`
(`dispatching`, `dispatched`, `failed`) and an `error`. The plan page lists the dispatches under **Runs in CI**,
with a link to the pipeline when the CI answered with a URL on the configured origin.

### Variables and refs are allowlists

A plan start can pass only what the project allows in `rerun`, so nobody who may start a plan can send arbitrary
variables or run another branch. Settings › Integrations › Rerun in CI edits both lists; they need
`settings:write`.

```json
{
  "rerun": {
    "provider": "github",
    "url": "https://api.github.com",
    "repository": "acme/shop",
    "workflow": "tests.yml",
    "ref": "main",
    "tokenSecretRef": "ci-token",
    "refs": ["main", "release/*"],
    "variables": [
      {"name": "BROWSER", "values": ["chromium", "firefox"], "default": "chromium", "description": "Browser"},
      {"name": "SUITE"}
    ]
  }
}
```

- `variables` (at most 20): `name` is `^[A-Z][A-Z0-9_]{0,63}$` and does not start with `FASTPROOF_`. With `values` (1 to
  50) only those values are accepted; without, any text of 1 to 256 bytes without control characters. `default`
  must be one of `values`; it is sent when the start does not set the variable.
- `refs` (at most 20 branch patterns, `*` within one path segment, `**` across `/`): the branch or tag a start may
  choose. The configured `ref` (default `main`) is always allowed and is used when the start names none. A
  webhook gets no ref.

A variable that is not listed, or a value that is not in its list, is refused with `400` `ci_variable_not_allowed`
(`details.name` names it); a ref that matches no pattern with `400` `ci_ref_not_allowed`. Nothing is started
then. Variables of an environment override those of the start, and the defaults fill the rest.

!!! note "Adding variables or refs while old versions still run"
    Saving `rerun.variables` or `rerun.refs` is refused with `409` `upgrade_in_progress` until every replica of
    the server runs this version (see [Upgrading](../operations/upgrading.md)).

### API

```json
POST /api/v1/projects/shop/test-plans/{id}/launches
{
  "name": "Nightly",
  "includeAutomated": true,
  "includeManual": false,
  "ci": {
    "dispatch": true,
    "ref": "release/2.14",
    "variables": {"SUITE": "smoke"},
    "environments": [{"name": "stage"}, {"name": "prod", "variables": {"BROWSER": "firefox"}}]
  }
}
```

The answer is `PlanLaunch`: `launches` (one per environment; `launch` is the first), `dispatches` and `skips`.
`GET /api/v1/projects/{key}/test-plans/{id}/dispatches` lists the dispatches of a plan (`cases:read`), newest
first. Callers without `settings:write` get generic error texts and only the pipeline id as `ciRef`.

Other answers: `409` `conflict` when the project has no `rerun` settings, `409` `plan_has_no_automated_cases`,
`403` without `launches:rerun`, and `429` when the project made 60 CI triggers in the last hour (reruns and plan
dispatches count together; every attempt counts) or the plan was started in CI less than 10 seconds ago. With
several environments each launch gets its own generated external id, so `externalId` is refused. Every
dispatch is written to the audit log as `testPlan.dispatch` with the plan, launch, environment, provider, ref,
the **names** of the variables (never values) and the target, never a token.

```sh
fastproof plan run --url https://fastproof.example.com --project shop --plan <plan id> \
  --env stage --env prod --var BROWSER=firefox --ref release/2.14
```

The command prints the external id of every launch it started and exits with `1` when a dispatch failed (the
launches stay). It creates no run items for manual cases unless you add `--with-manual`.

### What the CI receives

The same names as a rerun (`fastproof_plan_id` or `FASTPROOF_PLAN_ID`, `fastproof_launch` or `FASTPROOF_LAUNCH`,
`fastproof_source_launch`, `fastproof_url`), so a rerun workflow can serve plans, plus:

| | GitHub | GitLab | Webhook |
|---|---|---|---|
| allowed variables | one input `fastproof_variables`, a JSON object (sent only when it is not empty) | `variables[NAME]` for each | `variables` object |
| environment | `FASTPROOF_ENVIRONMENT` inside `fastproof_variables` | `variables[FASTPROOF_ENVIRONMENT]` | `environment` and `FASTPROOF_ENVIRONMENT` in `variables` |
| ref | `ref` of the dispatch | `ref` of the trigger | not sent |
| event | | | `"event": "plan"`, header `X-FastProof-Event: plan`, `"mode": "plan"` |

The launch already exists when the CI starts and carries the environment as its environment key, so the
reporter only has to join it by the external id and `FASTPROOF_ENVIRONMENT`. Take the same care as for a rerun:
declare the inputs, pass them through `env:`, validate every value and take the FastProof URL from a repository
variable.

GitHub Actions:

```yaml
on:
  workflow_dispatch:
    inputs:
      fastproof_plan_id: {type: string}
      fastproof_launch: {type: string}
      fastproof_source_launch: {type: string}
      fastproof_variables: {type: string, default: '{}'}

jobs:
  plan:
    runs-on: ubuntu-latest
    env:
      LAUNCH: ${{ inputs.fastproof_launch }}
      VARIABLES: ${{ inputs.fastproof_variables }}
      FASTPROOF_URL: ${{ vars.FASTPROOF_URL }}
      FASTPROOF_PROJECT: shop
      FASTPROOF_TOKEN: ${{ secrets.FASTPROOF_TOKEN }}
    steps:
      - run: |
          [[ "$LAUNCH" =~ ^plan-[0-9a-f-]{36}$ ]] || exit 1
          BROWSER=$(jq -r '.BROWSER // "chromium"' <<<"$VARIABLES")
          [[ "$BROWSER" =~ ^(chromium|firefox)$ ]] || exit 1
          FASTPROOF_ENVIRONMENT=$(jq -r '.FASTPROOF_ENVIRONMENT // ""' <<<"$VARIABLES")
          [[ "$FASTPROOF_ENVIRONMENT" =~ ^[A-Za-z0-9._-]{0,64}$ ]] || exit 1
          fastproof testplan --launch "$LAUNCH" > testplan.json
          export ALLURE_TESTPLAN_PATH="$PWD/testplan.json" FASTPROOF_LAUNCH="$LAUNCH" FASTPROOF_ENVIRONMENT BROWSER
          npm test
```

GitLab CI (trigger variables arrive as environment variables of a pipeline whose source is `trigger`):

```yaml
plan-tests:
  rules:
    - if: '$CI_PIPELINE_SOURCE == "trigger" && $FASTPROOF_PLAN_ID'
  variables:
    FASTPROOF_PROJECT: shop
  script:
    - fastproof testplan --launch "$FASTPROOF_LAUNCH" > testplan.json
    - export ALLURE_TESTPLAN_PATH="$PWD/testplan.json"
    - npm test
```

`FASTPROOF_URL` and `FASTPROOF_TOKEN` come from the project's CI variables, not from the trigger. The `BROWSER`
and `SUITE` variables of the example above arrive as `$BROWSER` and `$SUITE`.

### Plan check

When a launch started from a plan finishes, FastProof compares its results with the plan: results whose test is
not linked to an automated case of the plan (`extra`) and automated cases of the plan without a result
(`missing`). The launch page shows a banner when an extra result exists or nothing of a non-empty plan ran
(`GET /api/v1/projects/{key}/launches/{id}/plan-check`, `launches:read`), and the `launchFinished` notification
gets one more line.

## Security

- The CI is called once per request, without retries and without following redirects, and only through the
  [egress allowlist](../operations/configuration.md); the token goes only to the origin and target it is bound to.
- Every request names at most 1000 results.
- Every outcome, also a refusal (limits, missing settings, no tests, a CI error), is written to the audit log
  (`launch.rerun`, and `testPlan.dispatch` for a plan start) with the plan, the number of tests, the status and error, and the target as dispatched
  (provider, repository, workflow, ref, origin and secret names), never a token.
- Test plans expire after 90 days like other plans; rerun records are deleted with their launch.
