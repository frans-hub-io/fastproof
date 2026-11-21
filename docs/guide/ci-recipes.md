# CI recipes

The `fastproof` binary is the server and the CLI. In CI it needs three settings, best given as CI variables:

| Variable | Value |
|---|---|
| `FASTPROOF_URL` | Server URL, for example `https://fastproof.example.com` |
| `FASTPROOF_TOKEN` | An API token with the `uploader` role on the project (masked, protected) |
| `FASTPROOF_PROJECT` | Project key, for example `web` |
| `FASTPROOF_LAUNCH` | Optional external launch id; jobs with the same id report into one launch |

The CLI detects CI metadata (pipeline, job, URL, branch, commit) from the standard variables of GitLab CI,
GitHub Actions and Jenkins, so launches link back to the pipeline and history is filtered per branch.

Two ways to report:

- **`fastproof upload <path>`** after the tests: uploads a directory or file (allure-results, JUnit XML,
  CTRF, Playwright JSON) in one request.
- **`fastproof watch <dir>`** during the tests: uploads new result files as they appear, so the launch is
  visible live in the UI. Allure temporary files are ignored. Stop it when the tests end; it sends what is left
  and exits.

For shards, start the launch once, let every shard upload into it, and finish it at the end
(`fastproof launch start` and `fastproof launch finish`). Run `fastproof <command> --help` for all options.

!!! note "Getting the CLI"
    The CLI is a static Linux binary in the image `docker.io/franshub/fastproof`. Add it to your test image
    with `COPY --from=docker.io/franshub/fastproof:0.1.0-dev1 /usr/local/bin/fastproof /usr/local/bin/fastproof`,
    or copy it out with `docker create` and `docker cp` as in the recipes below. The image itself has no
    shell, so it cannot be the job image of a CI system that runs shell scripts.

## GitLab CI

```yaml
variables:
  FASTPROOF_URL: https://fastproof.example.com
  FASTPROOF_PROJECT: web
  FASTPROOF_LAUNCH: gitlab-$CI_PIPELINE_ID

stages: [test, report]

e2e:
  stage: test
  image: registry.example.com/qa/playwright-with-fastproof:1.65.0
  parallel: 4
  script:
    - fastproof launch start
    - fastproof watch allure-results &
    - watch_pid=$!
    - npx playwright test --shard=$CI_NODE_INDEX/$CI_NODE_TOTAL || status=$?
    - kill -INT "$watch_pid"; wait "$watch_pid" || true
    - exit ${status:-0}

report:
  stage: report
  image: registry.example.com/qa/playwright-with-fastproof:1.65.0
  when: always
  script:
    - fastproof launch finish
```

`registry.example.com/qa/playwright-with-fastproof` is your test image with the CLI copied in:

```dockerfile
FROM mcr.microsoft.com/playwright:v1.65.0-noble
COPY --from=docker.io/franshub/fastproof:0.1.0-dev1 /usr/local/bin/fastproof /usr/local/bin/fastproof
```

Store `FASTPROOF_TOKEN` as a masked and protected CI/CD variable. Without `watch`, replace the three watch
lines with `fastproof upload allure-results` in an `after_script`, so it also runs when tests fail.

## GitHub Actions

```yaml
name: e2e

on: [push, pull_request]

permissions:
  contents: read

jobs:
  e2e:
    runs-on: ubuntu-24.04
    strategy:
      matrix:
        shard: [1, 2, 3, 4]
    env:
      FASTPROOF_URL: https://fastproof.example.com
      FASTPROOF_TOKEN: ${{ secrets.FASTPROOF_TOKEN }}
      FASTPROOF_PROJECT: web
      FASTPROOF_LAUNCH: github-${{ github.run_id }}-${{ github.run_attempt }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
      - name: install fastproof CLI
        run: |
          mkdir -p "$HOME/.local/bin"
          id=$(docker create docker.io/franshub/fastproof:0.1.0-dev1)
          docker cp "$id:/usr/local/bin/fastproof" "$HOME/.local/bin/fastproof"
          docker rm "$id"
          echo "$HOME/.local/bin" >> "$GITHUB_PATH"
      - run: fastproof launch start
      - run: npm ci && npx playwright install --with-deps
      - name: tests
        run: npx playwright test --shard=${{ matrix.shard }}/4
      - name: upload results
        if: always()
        run: fastproof upload allure-results

  finish:
    needs: e2e
    if: always()
    runs-on: ubuntu-24.04
    env:
      FASTPROOF_URL: https://fastproof.example.com
      FASTPROOF_TOKEN: ${{ secrets.FASTPROOF_TOKEN }}
      FASTPROOF_PROJECT: web
      FASTPROOF_LAUNCH: github-${{ github.run_id }}-${{ github.run_attempt }}
    steps:
      - name: install fastproof CLI
        run: |
          mkdir -p "$HOME/.local/bin"
          id=$(docker create docker.io/franshub/fastproof:0.1.0-dev1)
          docker cp "$id:/usr/local/bin/fastproof" "$HOME/.local/bin/fastproof"
          docker rm "$id"
          echo "$HOME/.local/bin" >> "$GITHUB_PATH"
      - run: fastproof launch finish
```

The checkout action is pinned by commit SHA (v7.0.1); pin every action that way. Pull requests from forks have no access to secrets, so the
upload step is skipped there unless you add a condition on `secrets.FASTPROOF_TOKEN`.

For live reporting, start `fastproof watch allure-results &` before the tests in the same step and stop it with
`kill -INT` afterwards, as in the GitLab example.

## Jenkins

Declarative pipeline on an agent whose image contains the CLI:

```groovy
pipeline {
  agent { docker { image 'registry.example.com/qa/playwright-with-fastproof:1.65.0' } }
  environment {
    FASTPROOF_URL     = 'https://fastproof.example.com'
    FASTPROOF_PROJECT = 'web'
    FASTPROOF_TOKEN   = credentials('fastproof-token')
    FASTPROOF_LAUNCH  = "jenkins-${env.JOB_NAME}-${env.BUILD_NUMBER}"
  }
  stages {
    stage('Test') {
      steps {
        sh 'fastproof launch start'
        sh '''
          fastproof watch allure-results &
          watch_pid=$!
          status=0
          npx playwright test || status=$?
          kill -INT "$watch_pid"; wait "$watch_pid" || true
          exit $status
        '''
      }
    }
  }
  post {
    always {
      sh 'fastproof launch finish'
    }
  }
}
```

`credentials('fastproof-token')` reads a "Secret text" credential and masks it in the log.

## Which files to upload

| Tool | Output to upload |
|---|---|
| Any Allure adapter (Java, Python, JavaScript, .NET, Go, ...) | the `allure-results` directory |
| Maven Surefire / Gradle | `target/surefire-reports/` or `build/test-results/test/` (JUnit XML) |
| pytest | `--junitxml=junit.xml`, or `--alluredir=allure-results` with `allure-pytest` |
| Playwright | `--reporter=json` output, `--reporter=junit`, or `allure-playwright` |
| Vitest, gotestsum and others | their JUnit XML file |
| CTRF reporters | `ctrf/ctrf-report.json` |

Attachments referenced by the reports (screenshots, videos, traces) are uploaded with them when they are in the
uploaded directory or archive.
