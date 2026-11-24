using System;
using System.IO;
using System.Net.Http;
using System.Text.Json;
using System.Threading.Tasks;
using FransHub.FastProof;
using NUnit.Framework;

namespace Samples;

public static class State
{
    public static int Bump(string name)
    {
        var dir = Environment.GetEnvironmentVariable("SAMPLE_STATE_DIR") ?? Path.GetTempPath();
        var file = Path.Combine(dir, name);
        var n = File.Exists(file) ? int.Parse(File.ReadAllText(file)) + 1 : 1;
        File.WriteAllText(file, n.ToString());
        return n;
    }
}

[TestFixture]
[Category("checkout")]
public class CheckoutTests
{
    [Test]
    [FastProofId("CASE-1")]
    [FastProofLink("https://tracker.example.com/CASE-1", Name = "CASE-1", Type = "tms")]
    public void PassesWithStepsLogsAndAttachments()
    {
        var secret = Environment.GetEnvironmentVariable("SAMPLE_API_TOKEN");
        Console.WriteLine($"starting with token {secret}");
        FastProofContext.Step("outer", () =>
        {
            FastProofContext.Step("inner", () => Console.WriteLine("inside the inner step"));
            FastProofContext.Log("a log line through FastProofContext.Log", "warn");
        });
        FastProofContext.Attach($"note-{secret}.txt", $"body with {secret}", "text/plain");
        Assert.Pass();
    }

    [Test]
    public void FailsAssertion()
    {
        Console.Error.WriteLine("about to fail");
        Assert.That(2 + 2, Is.EqualTo(5), "arithmetic");
    }

    [Test]
    public void ThrowsAPlainError()
    {
        throw new InvalidOperationException("boom");
    }

    [Test]
    [Retry(3)]
    public void FlakyPassesOnThirdTry()
    {
        var n = State.Bump("flaky");
        Console.WriteLine($"try {n}");
        Assert.That(n, Is.GreaterThanOrEqualTo(3));
    }

    [Test]
    [Retry(2)]
    public void FailsEveryTime()
    {
        Assert.Fail("never passes");
    }

    [Test]
    [Ignore("not today")]
    public void Skipped()
    {
    }

    [TestCase(1, 2, 3)]
    [TestCase(2, 2, 4)]
    [TestCase(2, 2, 5)]
    public void AddsNumbers(int a, int b, int expected)
    {
        Assert.That(a + b, Is.EqualTo(expected));
    }

    [Test]
    public async Task TraceparentReachesTheBackend()
    {
        await Task.Delay(20);
        var headers = FastProofContext.Trace.Headers();
        FastProofContext.Attach("seen-headers.json", JsonSerializer.Serialize(headers), "application/json");
        Assert.That(headers["traceparent"], Does.StartWith("00-"));
    }
}

[TestFixture]
[Category("parallel")]
public class ParallelOneTests
{
    [Test]
    public async Task Waits()
    {
        Console.WriteLine("parallel one");
        await Task.Delay(300);
    }
}

[TestFixture]
[Category("parallel")]
public class ParallelTwoTests
{
    [Test]
    public async Task Waits()
    {
        Console.WriteLine("parallel two");
        await Task.Delay(300);
    }

    public class Nested
    {
        [Test]
        public void NestedClassTest()
        {
        }
    }
}
