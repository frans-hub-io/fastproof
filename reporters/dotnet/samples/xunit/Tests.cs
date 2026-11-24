using System;
using System.Text.Json;
using System.Threading.Tasks;
using FransHub.FastProof;
using Xunit;

namespace Samples;

[Trait("Category", "checkout")]
public class CheckoutTests
{
    private readonly ITestOutputHelper _output;

    public CheckoutTests(ITestOutputHelper output) => _output = output;

    [Fact]
    [FastProofId("CASE-1")]
    [FastProofLink("https://tracker.example.com/CASE-1", Name = "CASE-1", Type = "tms")]
    public void PassesWithStepsLogsAndAttachments()
    {
        var secret = Environment.GetEnvironmentVariable("SAMPLE_API_TOKEN");
        _output.WriteLine($"starting with token {secret}");
        Console.WriteLine("console line");
        FastProofContext.Step("outer", () =>
        {
            FastProofContext.Step("inner", () => _output.WriteLine("inside the inner step"));
            FastProofContext.Log("a log line through FastProofContext.Log", "warn");
        });
        FastProofContext.Attach($"note-{secret}.txt", $"body with {secret}", "text/plain");
    }

    [Fact]
    public void FailsAssertion()
    {
        _output.WriteLine("about to fail");
        Assert.Equal(5, 2 + 2);
    }

    [Fact]
    public void ThrowsAPlainError()
    {
        throw new InvalidOperationException("boom");
    }

    [Fact(Skip = "not today")]
    public void Skipped()
    {
    }

    [Fact]
    public void SkipsAtRunTime()
    {
        Assert.Skip("needs a feature flag");
    }

    [Theory]
    [InlineData(1, 2, 3)]
    [InlineData(2, 2, 4)]
    [InlineData(2, 2, 5)]
    public void AddsNumbers(int a, int b, int expected)
    {
        Assert.Equal(expected, a + b);
    }

    [Fact]
    public async Task TraceparentReachesTheBackend()
    {
        await Task.Delay(20, TestContext.Current.CancellationToken);
        var headers = FastProofContext.Trace.Headers();
        FastProofContext.Attach("seen-headers.json", JsonSerializer.Serialize(headers), "application/json");
        Assert.StartsWith("00-", headers["traceparent"]);
    }
}

public class ParallelOneTests
{
    [Fact]
    public async Task Waits()
    {
        await Task.Delay(300, TestContext.Current.CancellationToken);
    }
}

public class ParallelTwoTests
{
    [Fact]
    public async Task Waits()
    {
        await Task.Delay(300, TestContext.Current.CancellationToken);
    }

    public class Nested
    {
        [Fact]
        public void NestedClassTest()
        {
        }
    }
}
