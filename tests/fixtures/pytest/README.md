# pytest fixtures

Status: **generated** by `tests/fixtures/generators/pytest/generate.sh` (real writers, not hand-edited;
only path/host sanitization is applied).

Versions: Python 3.11, pytest 9.2.0, allure-pytest 2.16.2 (allure-python-commons 2.16.2),
pytest-rerunfailures 16.7.

Regenerate:

```sh
WORK=/path/to/scratch PYTHON=python3.11 tests/fixtures/generators/pytest/generate.sh
```

`VENV=/path/to/venv` reuses an existing virtualenv. The script runs pytest twice
(pytest writes one JUnit file per run): run 1 writes `allure-results/` and `junit-xunit2.xml`,
run 2 writes `junit-xunit1.xml`. Absolute paths are rewritten to `/builds/example/fastproof-samples`
(and the venv to `/builds/example/venv`), the host name to `runner.example.com`.

| File | Covers |
| --- | --- |
| `allure-results/*-result.json` | 16 results: pass with nested steps and text/json/png attachments, labels (epic, feature, story, severity, tag, owner, `as_id` from `allure.id`), links (link, issue, tms), docstring description, `allure.title`; failed assertion; broken (TypeError); two skips (`pytest.skip`, `@pytest.mark.skip`); 3 parametrized cases with dynamic parameters (`masked`, `hidden`, `excluded`); a flaky test (failed, then passed on rerun); a test failing on all 3 attempts; xfail (`skipped`); xpass (`passed`); broken in fixture setup |
| `allure-results/*-container.json` | fixture befores/afters with steps and attachments (module-scoped `api_client`, function `cart`, failing `database` setup) and parameter pseudo-fixtures without children |
| `allure-results/*-attachment.*` | text, json, png attachments and captured stdout |
| `allure-results/environment.properties`, `categories.json` | copied by the script, as users do; allure-pytest does not write them |
| `junit-xunit2.xml` | default `junit_family=xunit2`, `junit_logging=all` (system-out / system-err per testcase), rerun attempts as extra testcases |
| `junit-xunit1.xml` | `junit_family=xunit1` (adds `file` and `line` attributes), no logging |
