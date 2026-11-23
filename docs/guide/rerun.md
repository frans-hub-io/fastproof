# Rerun from a launch

**Rerun failed** and **Rerun selected** on a launch page make a test plan (Allure `testplan.json`) of the tests
to run again and trigger the CI that produced the launch. The CI is told which plan to run and which external
id its new launch must use; the new launch then shows **Rerun of #N** and the original lists it.

The CI must run only the tests of the plan. Allure adapters do this when `ALLURE_TESTPLAN_PATH` points at the
file (see [Reporters](reporters.md)).

## Settings

The CI is configured per project in **Settings** › **Integrations** › **Rerun in CI**, or in the `rerun` key of
`PUT /api/v1/projects/{key}/settings` (needs `settings:write`). The token is a [project secret](notifications.md#project-secrets)
with purpose `ci`, bound to the origin of `url`; a secret of another purpose or origin is refused when the
settings are saved and again when it is used.

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
| `github` | `POST /repos/{repository}/actions/workflows/{workflow}/dispatches` (`workflow_dispatch`) with the token as a bearer token | `url` (API URL), `repository`, `workflow`, `ref` (default `main`), `tokenSecretRef` |
| `gitlab` | `POST /api/v4/projects/{repository}/trigger/pipeline` with a pipeline trigger token | `url`, `repository`, `ref`, `tokenSecretRef` (the trigger token) |
| `webhook` | `POST` of a JSON document to `url`; an optional `tokenSecretRef` is sent as `Authorization: Bearer` | `url`, optional `tokenSecretRef` |

What the CI receives:

| | GitHub input | GitLab variable | Webhook field |
|---|---|---|---|
| the test plan | `fastproof_plan_id` | `FASTPROOF_PLAN_ID` | `planId`, `planUrl`, and `testplan` (up to 2000 tests) |
| external id of the new launch | `fastproof_launch` | `FASTPROOF_LAUNCH` | `newLaunchExternalId` |
| the original launch | `fastproof_source_launch` | `FASTPROOF_SOURCE_LAUNCH` | `launch` |
| FastProof URL (when `FASTPROOF_PUBLIC_URL` is set) | `fastproof_url` | `FASTPROOF_URL` | `planUrl` |

A GitHub workflow must declare these inputs under `on.workflow_dispatch.inputs`, otherwise GitHub refuses the
dispatch. A job fetches the plan and starts the launch with the given external id:

```yaml
- run: |
    fastproof testplan --plan "${{ inputs.fastproof_plan_id }}" > testplan.json
    ALLURE_TESTPLAN_PATH=testplan.json FASTPROOF_LAUNCH="${{ inputs.fastproof_launch }}" npm test
  env:
    FASTPROOF_URL: ${{ inputs.fastproof_url }}
    FASTPROOF_TOKEN: ${{ secrets.FASTPROOF_TOKEN }}
```

Joining a launch by external id is the way every reporter already joins a launch, so the link needs no extra
step in the CI.

## API and CLI

`POST /api/v1/projects/{key}/launches/{id}/rerun` needs `launches:write`:

```json
{"mode": "failed", "statuses": ["failed", "broken"]}
{"mode": "selected", "resultIds": ["..."]}
```

It answers `201` with the rerun (`id`, `externalId`, `planId`, `testCount`, `status`, `ciRef`). `409` means the
project has no `rerun` settings or no test would run; `429` that the launch was rerun less than 10 seconds ago,
20 times already, or the project had 60 reruns in the last hour; `502` that the CI refused or was unreachable
(the rerun is kept with the reason, which holds no URL and no secret).
`GET /api/v1/projects/{key}/launches/{id}/reruns` lists the reruns of a launch with the launches they started
and the rerun that started this launch (`launches:read`).

```sh
fastproof rerun --url https://fastproof.example.com --project shop --launch <id or external id>
fastproof rerun --launch <id> --result <result id> --result <result id>
```

## Security

- The CI is called once per request, without retries and without following redirects, and only through the
  [egress allowlist](../operations/configuration.md); the token goes only to the origin it is bound to.
- Every request, also a refused one, counts against the limits above; a request names at most 1000 results.
- Every rerun is written to the audit log (`launch.rerun`) with the plan, the number of tests and the outcome,
  never the token.
- Test plans expire after 90 days like other plans; rerun records are deleted with their launch.
