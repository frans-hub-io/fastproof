# fastproof-allure-pytest

Streams allure-pytest results to FastProof while the tests run, keeping `--alluredir` output. Needs
`allure-pytest` and `pytest-fastproof` (the shared core); with `--alluredir` it replaces `pytest-fastproof`'s own
reporting, without it `pytest-fastproof` reports as before. Settings: `FASTPROOF_*` as in
[reporters/PROTOCOL.md](../PROTOCOL.md).

```sh
pip install fastproof-allure-pytest
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_PROJECT=shop pytest --alluredir=allure-results
```

Test code: `fastproof_allure_pytest.trace().headers()` or the `fastproof_trace` fixture. `FASTPROOF_ALLURE_TRACE_LABEL=false`
stops the `traceparent` label in the Allure result.

Development: install `../pytest` and this directory in editable mode, then `pytest`.
`reporters/integration/test_allure_pytest.py` runs a sample against a real server.
