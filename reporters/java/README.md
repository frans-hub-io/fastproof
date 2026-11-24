# FastProof reporters for the JVM

Live results, logs and attempts for JUnit 5 (Jupiter) and TestNG. Maven multi-module build, Java 17 baseline
(tested on JDK 17 and 25), group `io.franshub`, no runtime dependencies beyond the JDK.

| Module | What it is |
|---|---|
| [`core`](core) `fastproof-core` | Config, HTTP client (`java.net.http`), spool, masking, CI detection, trace context, attempts, stdout/stderr and `java.util.logging` capture, test plans, the `flush` command. Has `FastProof` for test code (`step`, `attach`, `attachFile`, `trace`, `log`). |
| [`junit5`](junit5) `fastproof-junit5` | Listeners of the JUnit Platform, found by `ServiceLoader`; optional `FastProofExtension` for argument values. |
| [`testng`](testng) `fastproof-testng` | TestNG listeners, found by `ServiceLoader`; `Retry` analyzer and an opt-in global retry. |
| [`samples`](samples) | Two sample test projects (profiles `junit` and `testng`) used by the integration tests. |

```xml
<dependency>
  <groupId>io.franshub</groupId>
  <artifactId>fastproof-junit5</artifactId>
  <version>0.1.0</version>
  <scope>test</scope>
</dependency>
```

```sh
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_PROJECT=shop FASTPROOF_TOKEN=... mvn test
```

The user guide is [Reporters](../../docs/guide/reporters.md#junit-5-and-testng); the wire contract is
[PROTOCOL.md](../PROTOCOL.md). Settings are the `FASTPROOF_*` variables or `fastproof.*` system properties
(`-Dfastproof.url=...`, which win), plus `FASTPROOF_CAPTURE_OUTPUT=false` (do not wrap `System.out` and
`System.err`), `FASTPROOF_LOG_LEVEL`, `FASTPROOF_FINISH_LAUNCH` (`auto`, `always`, `never`) and `fastproof.testng.retries`.

## How a test maps to attempts

- JUnit: the unique id of the test is the attempt key; running the same test again in the same JVM (Surefire
  `rerunFailingTestsCount`, a second launcher execution) makes the next attempt of the same result. Template
  invocations (`@ParameterizedTest`, `@RepeatedTest`), dynamic tests and factories have their own unique ids, so
  each is a result of its own.
- TestNG: a retried test (`IRetryAnalyzer`; TestNG reports the failed try as skipped with `wasRetried()`) continues the
  attempt count with the real status of the try; a repeated test of a method with `invocationCount > 1` is the
  next invocation (a result of its own, with the parameter `invocation`), and a test that runs again in the same
  JVM with `invocationCount = 1` is the next attempt. Surefire's `rerunFailingTestsCount` was only verified with
  JUnit; with TestNG use `Retry` or your own `IRetryAnalyzer`.
- Client ids are `ju-<runId>-<sha1(key)[:16]>-<attempt>` and `tn-...`; `runId` is 12 random hex characters per JVM.

## Output capture

`System.out` and `System.err` are wrapped once per JVM (the original streams still get every byte); lines are
attributed to the attempt running on the writing thread or on a thread that attempt started. Records of
`java.util.logging` (and so `System.Logger` with the default backend) are copied through a handler on the root
logger. SLF4J, Log4j2 and Logback writing to the console are captured as stdout. When the server has no live
logs, the log records go to the end of the result's `stdout` after a `--- log ---` line.

## Development

```sh
cd reporters/java
mvn install                       # unit tests of the three modules against a fake server
cd ../.. && FASTPROOF_TEST_MVN=$(which mvn) FASTPROOF_TEST_SERVER_BIN=target/debug/fastproof \
  FASTPROOF_TEST_DATABASE_URL=postgres://user@127.0.0.1:5432/postgres \
  pytest -p no:fastproof reporters/integration/test_java.py
```

Apache-2.0. Publishing to Maven Central is not set up yet.
