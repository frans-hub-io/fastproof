# JUnit 5 / JUnit Platform fixtures

Status: **generated** by real writers (not handwritten).

## Versions

| Component | Version |
|---|---|
| JDK | Temurin 25.0.5 (compiled with `release 21`) |
| Apache Maven | 3.9.16 |
| junit-jupiter, junit-jupiter-params, junit-platform-launcher | 6.1.3 |
| io.qameta.allure:allure-jupiter | 3.1.1 (`allure-junit5` was renamed to `allure-jupiter`; `allure-junit5` stops at 2.35.x as a relocation POM) |
| maven-surefire-plugin | 3.6.1 (`rerunFailingTestsCount=2`, `testFailureIgnore=true`, `redirectTestOutputToFile=false`) |
| maven-compiler-plugin | 3.16.0 |
| maven-resources-plugin | 3.5.0 |
| slf4j-nop | 2.0.20 (silences the SLF4J warning that would otherwise land in the first test's `system-err`) |

## Generation

```sh
MVN=/path/to/mvn MAVEN_REPO=/path/to/m2 WORK=/path/to/workdir \
  tests/fixtures/generators/junit5/generate.sh
```

`MVN` defaults to `mvn`, `MAVEN_REPO` defaults to `$WORK/m2-repository`, `WORK` defaults to a fresh `mktemp -d`.
The script copies the generator into `$WORK/fastproof-samples`, runs `mvn test`, copies the outputs here and
replaces machine-specific paths, host and user names (`/builds/example/fastproof-samples`,
`/builds/example/.m2/repository`, `/opt/java/openjdk`, `runner.example.com`, `runner`).
UUIDs, timestamps, PIDs and durations change on every run.

## Files

- `allure-results/` - raw output of allure-jupiter: `*-result.json` (one per attempt, so retried tests
  appear several times), `*-container.json` (fixtures), `*-attachment.{txt,json,png,httpexchange}`.
- `surefire-reports/TEST-com.example.samples.CheckoutTest.xml`, `TEST-com.example.samples.RetryTest.xml` -
  Surefire XML (schema `surefire-test-report.xsd` version 3.0.2) including the `<properties>` block.

## Covered cases

`com.example.samples.CheckoutTest` (`@BeforeAll`, `@BeforeEach`, `@AfterEach` with steps):

| Test | Result | Covers |
|---|---|---|
| `payForCart` | passed | annotations `@AllureId @Epic @Feature @Story @Severity @Owner @Tag @Link @Issue @TmsLink @Description @DisplayName`, nested steps (2 levels), text/json/png attachments, stdout (dropped by Surefire, see below) |
| `discountApplied` | failed x3 (`rerunFailure`) | runtime API `Allure.story/label/issue/tms/link/parameter/description`, HTTP exchange attachment, `assertEquals` expected/actual, stdout + stderr, `[[ATTACHMENT|attachments/screenshot.png]]` line |
| `unknownItem` | broken x3 (`rerunError`) | `IllegalStateException` |
| `payWithGiftCard` | skipped | `@Disabled("...")` |
| `payInForeignCurrency` | skipped | `Assumptions.assumeTrue(false, "...")` |
| `priceOfItem` | 2 passed, 1 failed x3 | `@ParameterizedTest` + `@CsvSource`, rerun of a single invocation |

`com.example.samples.RetryTest` (no fixtures):

| Test | Result | Covers |
|---|---|---|
| `flakyGateway` | failed, then passed (`flakyFailure`) | flaky test via a file counter, failed step with trace, hidden excluded parameter `attempt` |
| `refundRejected` | failed x3 (`rerunFailure`) | always failing assertion, stdout per attempt |
| `cardVaultUnreachable` | broken x3 (`rerunError`) | always throwing, stderr per attempt |
