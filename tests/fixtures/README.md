# Test fixtures

There is no official schema for allure-results or JUnit XML; the writers are the contract. These fixtures were
produced by running the real frameworks with the generator projects in `generators/<tool>/` (exact versions
pinned there); outputs were only sanitized (paths rewritten to `/builds/example/fastproof-samples`, host to
`runner.example.com`, user to `runner`). Each tool directory has its own README with details. None of the
fixtures is handwritten.

| Tool | Versions | Outputs | Covers |
| --- | --- | --- | --- |
| pytest | Python 3.11, pytest 9.2.0, allure-pytest 2.16.2, pytest-rerunfailures 16.7 | `pytest/allure-results/`, `junit-xunit2.xml`, `junit-xunit1.xml` | pass, fail, broken, skip, xfail, xpass, parametrize with masked/hidden/excluded params, nested steps with text/json/png, fixtures (containers), setup failure, flaky and always-failing reruns, labels, links, environment.properties, categories.json |
| JUnit 5 | JDK 25.0.5, Maven 3.9.16, JUnit 6.1.3, allure-jupiter 3.1.1, Surefire 3.6.1 | `junit5/allure-results/`, `junit5/surefire-reports/` | annotations and runtime API, nested steps, http-exchange attachment, `@ParameterizedTest`, `@Disabled`, assumptions, `@BeforeAll/@BeforeEach/@AfterEach`, Surefire `flakyFailure`, `rerunFailure`, `rerunError`, stdout/stderr, `[[ATTACHMENT|...]]` |
| Playwright | Node 22, @playwright/test 1.65.0, allure-playwright 3.14.0, playwright-ctrf-json-reporter 0.2.0 | `playwright/allure-results/`, `results.json`, `junit.xml`, `ctrf/ctrf-report.json`, `test-results/` | two projects (environment dimension), retries (flaky and always failing), steps, attachments by path and inline body, skip/fixme/fail, annotations, tags, stdout/stderr |
| Vitest | Node 22, vitest 5.1.0, allure-vitest 3.14.0 (with @vitest/runner 4.1.12, see its README) | `vitest/allure-results/`, `vitest/junit.xml` | retries (one `failure` element per attempt in JUnit), TypeError, skip, todo, `test.each`, steps, attachments, labels |
| gotestsum | Go 1.27.3, gotestsum v1.13.0 | `gotestsum/junit.xml`, `junit-norerun.xml` | `--rerun-fails` duplicates, subtests, skip, panic |

Identity vectors for `crates/fastproof-model` are computed by the Allure 3 v3.33.0 source itself:
`crates/fastproof-model/tests/vectors/generate.sh`.
