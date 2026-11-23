# fastproof-allure-java

An allure-java 3.1.x lifecycle listener (registered through `ServiceLoader`) that streams results to FastProof
while JUnit 5 or TestNG tests run. allure-results are still written. Needs Java 17. Settings are `FASTPROOF_*`
variables or `fastproof.*` system properties; the wire contract is [reporters/PROTOCOL.md](../PROTOCOL.md).

Add the jar to the test classpath (see the docs page "Reporters"). `FastProof.trace()` gives the trace context of
the attempt on the current thread. The report is closed in a shutdown hook; the offline spool uses the shared
format, so `npx fastproof-playwright flush` or `python -m pytest_fastproof flush` sends it (the Java module has no
flush command of its own).

Development: `mvn test` (unit tests with a fake server); `samples/` holds JUnit 5 and TestNG samples that
`reporters/integration/test_allure_java.py` runs against a real server (`FASTPROOF_TEST_MVN` points to Maven).
