# FastProof reporters for .NET

Reporters that send .NET test runs to FastProof while they happen: a running result when a test starts, live
logs and steps, every retry as its own attempt, `traceparent` per attempt, secret masking at the source, and an
offline spool that never fails the test run. No telemetry, no dependency beyond the base class library (and the
test framework itself). The wire contract is [reporters/PROTOCOL.md](../PROTOCOL.md).

| Package | For | Targets |
|---|---|---|
| `FransHub.FastProof.Core` | the client, spool, masking, trace context and the calls tests make | net8.0, net10.0 |
| `FransHub.FastProof.NUnit` | NUnit 4 (tested with 4.6.1), including `[Retry]` and `[Repeat]` | net8.0, net10.0 |
| `FransHub.FastProof.Xunit` | xUnit.net v3 (tested with 4.0.2); v2 is not supported | net8.0, net10.0 |

## Use

```sh
dotnet add package FransHub.FastProof.NUnit
```

```csharp
using FransHub.FastProof.NUnit;
[assembly: FastProofReporter]
```

For xUnit v3 the package is `FransHub.FastProof.Xunit` and the attribute `FransHub.FastProof.Xunit.FastProofReporter`
(add `[assembly: CaptureConsole]` as well when tests write to the console).

```sh
FASTPROOF_URL=https://fastproof.example.com FASTPROOF_PROJECT=shop FASTPROOF_TOKEN=... dotnet test
```

The reporter stays off without `FASTPROOF_URL`. Settings are the `FASTPROOF_*` variables of PROTOCOL.md section 1;
the attribute also takes `Url` and `Project`. Several test projects of one `dotnet test` run are separate
processes: set `FASTPROOF_LAUNCH` (or run in CI, where the launch id comes from the pipeline) so they share one
launch.

Inside tests, `FastProofContext` (namespace `FransHub.FastProof`) offers:

```csharp
FastProofContext.Step("call the payment API", () => client.Post(url, FastProofContext.Trace.Headers()));
await FastProofContext.StepAsync("wait for the ledger", async () => await ledger.WaitAsync());
FastProofContext.Attach("response.json", json, "application/json");
FastProofContext.AttachFile("screenshots/pay.png");
FastProofContext.Log("paid", "info");
Process.Start(new ProcessStartInfo { Environment = { ["TRACEPARENT"] = FastProofContext.Trace.Traceparent } });
```

and the attributes `[FastProofId("CASE-7")]`, `[FastProofLabel("owner", "payments")]` and
`[FastProofLink("https://tracker.example.com/CASE-7", Name = "CASE-7", Type = "tms")]`.

## What is reported

- Theory rows, `[TestCase]` and `[TestCaseSource]` rows are separate results; their arguments are parameters.
- NUnit `[Retry]` and `[Repeat]` passes are attempts of one result (every attempt has its own status, error,
  output and trace id; mixed outcomes mark the result flaky). xUnit has no retries of its own; a third-party retry
  attribute that runs the test again reports attempts the same way.
- Labels: `framework`, `parentSuite` (namespace), `suite` (class), `subSuite` (nested classes), `thread`, `tag`
  (NUnit `[Category]`, xUnit `Category` trait), other NUnit `[Property]` values and xUnit traits by name.
- Console output, `ITestOutputHelper` output (xUnit, polled every 200 ms) and `FastProofContext.Log` are streamed
  live and are part of the result; secrets are masked before anything is queued, spooled or uploaded.
- Test plans: `FASTPROOF_TESTPLAN` (Allure `testplan.json`). Selectors are `Namespace.Type.Method` and
  `Namespace.Type#Method`, with or without arguments, the test's display name, or an id from `[FastProofId]`,
  the NUnit property or xUnit trait `fastproof.id`, `allure_id` or `as_id`. Tests the plan does not select are
  skipped and not reported.

## Known limits

- NUnit: a failing `[OneTimeSetUp]` is not reported as a result of its own (its tests are not started, so they
  do not appear); `TestContext.Out` writes are not captured (use `Console` or `FastProofContext.Log`).
- xUnit: tests with a static `Skip` never reach the extension points of the framework and are not reported;
  `Assert.Skip` at run time is reported as skipped.
- The spool sent by `flush` of the other reporters: `npx fastproof-playwright flush` or
  `python -m pytest_fastproof flush` send any spool session (the format is shared); the next run of this reporter
  sends it too.

## Development

```sh
dotnet test tests/FransHub.FastProof.Core.Tests
dotnet pack src/FransHub.FastProof.NUnit -c Release -o artifacts/nupkg
```

`samples/nunit` and `samples/xunit` hold the sample test projects that `reporters/integration/test_dotnet.py`
runs against a real server (`FASTPROOF_TEST_DOTNET` points to the `dotnet` launcher). The xUnit sample uses the
Microsoft.Testing.Platform runner (`global.json` in its folder), which xUnit v3 needs on the .NET 10 SDK.
