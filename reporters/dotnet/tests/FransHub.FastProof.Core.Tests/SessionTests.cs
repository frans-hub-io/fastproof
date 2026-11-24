using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;
using NUnit.Framework;

namespace FransHub.FastProof.Tests;

[TestFixture]
[NonParallelizable]
public class SessionTests
{
    private string _dir = string.Empty;
    private readonly List<string> _messages = new();

    [SetUp]
    public void SetUp()
    {
        _dir = Path.Combine(Path.GetTempPath(), "fp-dotnet-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_dir);
        _messages.Clear();
    }

    [TearDown]
    public void TearDown()
    {
        ReporterSession.Reset();
        try
        {
            Directory.Delete(_dir, true);
        }
        catch (IOException)
        {
        }
    }

    private ReporterSession Session(string url, params (string, string)[] extra)
    {
        var env = new Dictionary<string, string>
        {
            ["FASTPROOF_URL"] = url,
            ["FASTPROOF_PROJECT"] = "shop",
            ["FASTPROOF_TOKEN"] = "test-token-123456",
            ["FASTPROOF_SPOOL_DIR"] = Path.Combine(_dir, "spool"),
            ["FASTPROOF_FLUSH_TIMEOUT_MS"] = "5000",
            ["FASTPROOF_QUIET"] = "true",
            ["SAMPLE_API_TOKEN"] = "sample-secret-9999",
        };
        foreach (var (k, v) in extra)
        {
            env[k] = v;
        }
        var cfg = Config.Resolve(null, env, _dir);
        var s = ReporterSession.Create(cfg, m => { lock (_messages) { _messages.Add(m); } });
        Assert.That(s, Is.Not.Null);
        return s!;
    }

    private static TestDescriptor Test(string name, params (string, string)[] parameters)
    {
        var d = new TestDescriptor { Name = name, FullName = "Ns.Type." + name, Framework = "nunit" };
        d.Labels.Add(new LabelItem("suite", "Type"));
        foreach (var (k, v) in parameters)
        {
            d.Parameters.Add(new ParameterItem(k, v));
        }
        return d;
    }

    private static void WaitFor(Func<bool> cond, int ms = 10000)
    {
        var until = DateTime.UtcNow.AddMilliseconds(ms);
        while (!cond() && DateTime.UtcNow < until)
        {
            Thread.Sleep(25);
        }
    }

    [Test]
    public void LiveRunStreamsRunningResultLogsStepsAttachmentsAndMasksSecrets()
    {
        using var server = new FakeServer { ExpectToken = "test-token-123456" };
        var s = Session(server.Url);
        var a = s.Begin(Test("Pays"))!;
        s.Output(a, "paying with sample-secret-9999\n");
        FastProofContext.Step("outer", () => FastProofContext.Step("inner", () => FastProofContext.Log("inside")));
        FastProofContext.Attach("note-sample-secret-9999.txt", "body sample-secret-9999");
        s.End(a, new AttemptOutcome { Status = AttemptStatus.Failed, ErrorMessage = "failed with sample-secret-9999", ErrorTrace = "at X" });
        var summary = s.Close();

        Assert.That(summary!.Sent, Is.EqualTo(1));
        Assert.That(server.Launches.Single(), Does.StartWith("local-"));
        Assert.That(server.Running().Count(), Is.EqualTo(1));
        var r = server.Final().Single();
        Assert.That(r["id"]!.GetValue<string>(), Does.Match("^dn-[0-9a-f]{12}-[0-9a-f]{16}-0$"));
        Assert.That(r["status"]!.GetValue<string>(), Is.EqualTo("failed"));
        Assert.That(r["retry"]!.GetValue<int>(), Is.EqualTo(0));
        Assert.That(r["stop"], Is.Not.Null);
        Assert.That(r["traceparent"]!.GetValue<string>(), Does.StartWith("00-"));
        Assert.That(r["stdout"]!.GetValue<string>(), Is.EqualTo("paying with ****\n"));
        Assert.That(r["error"]!["message"]!.GetValue<string>(), Is.EqualTo("failed with ****"));
        Assert.That(r["steps"]![0]!["steps"]![0]!["name"]!.GetValue<string>(), Is.EqualTo("inner"));
        var att = r["attachments"]![0]!;
        Assert.That(att["name"]!.GetValue<string>(), Is.EqualTo("note-****.txt"));
        Assert.That(Encoding.UTF8.GetString(server.Attachments[att["id"]!.GetValue<string>()]), Is.EqualTo("body ****"));
        var logs = server.Logs.ToList();
        Assert.That(logs.Select(l => l["seq"]!.GetValue<int>()), Is.EqualTo(Enumerable.Range(0, logs.Count)));
        Assert.That(logs.Any(l => l["stream"]!.GetValue<string>() == "step" && l["message"]!.GetValue<string>().Contains("inner")), Is.True);
        Assert.That(logs.All(l => !l.ToJsonString().Contains("sample-secret-9999")), Is.True);
        Assert.That(server.Finished.Count, Is.EqualTo(1));
        Assert.That(server.Results.All(x => !x.ToJsonString().Contains("sample-secret-9999")), Is.True);
    }

    [Test]
    public void RetriesAreAttemptsOfOneResultWithTheirOwnIdAndTrace()
    {
        using var server = new FakeServer();
        var s = Session(server.Url);
        var statuses = new[] { AttemptStatus.Failed, AttemptStatus.Failed, AttemptStatus.Passed };
        foreach (var st in statuses)
        {
            var a = s.Begin(Test("Flaky"))!;
            s.End(a, new AttemptOutcome { Status = st });
        }
        s.Close();
        var finals = server.Final().OrderBy(r => r["retry"]!.GetValue<int>()).ToList();
        Assert.That(finals.Select(f => f["status"]!.GetValue<string>()), Is.EqualTo(new[] { "failed", "failed", "passed" }));
        Assert.That(finals.Select(f => f["retry"]!.GetValue<int>()), Is.EqualTo(new[] { 0, 1, 2 }));
        Assert.That(finals.Select(f => f["id"]!.GetValue<string>()).Distinct().Count(), Is.EqualTo(3));
        Assert.That(finals.Select(f => f["traceparent"]!.GetValue<string>().Split('-')[1]).Distinct().Count(), Is.EqualTo(3));
        Assert.That(finals.Select(f => f["fullName"]!.GetValue<string>()).Distinct().Count(), Is.EqualTo(1));
    }

    [Test]
    public void ParametersMakeSeparateTests()
    {
        using var server = new FakeServer();
        var s = Session(server.Url);
        foreach (var v in new[] { "1", "2" })
        {
            var a = s.Begin(Test("Adds", ("a", v)))!;
            s.End(a, new AttemptOutcome { Status = AttemptStatus.Passed });
        }
        s.Close();
        var finals = server.Final().ToList();
        Assert.That(finals.Select(f => f["retry"]!.GetValue<int>()), Is.EqualTo(new[] { 0, 0 }));
        Assert.That(finals.Select(f => f["parameters"]![0]!["value"]!.GetValue<string>()).OrderBy(x => x), Is.EqualTo(new[] { "1", "2" }));
    }

    [Test]
    public void IdenticalAttachmentsAreUploadedOnce()
    {
        using var server = new FakeServer();
        var s = Session(server.Url);
        for (var i = 0; i < 3; i++)
        {
            var a = s.Begin(Test("T" + i))!;
            FastProofContext.Attach("same.bin", new byte[] { 1, 2, 3, 4, 5 });
            s.End(a, new AttemptOutcome { Status = AttemptStatus.Passed });
            WaitFor(() => server.Final().Count() > i);
        }
        s.Close();
        Assert.That(server.AttachmentUploads, Is.EqualTo(1));
        Assert.That(server.Final().Select(f => f["attachments"]![0]!["id"]!.GetValue<string>()).Distinct().Count(), Is.EqualTo(1));
    }

    [Test]
    public void AServerThatRejectsRunningLinesAndHasNoLogsEndpointStillGetsEverything()
    {
        using var server = new FakeServer { RejectRunning = true, NoLogs = true };
        var s = Session(server.Url);
        var a = s.Begin(Test("Old"))!;
        s.Output(a, "hello\n");
        FastProofContext.Log("a log line");
        s.End(a, new AttemptOutcome { Status = AttemptStatus.Passed });
        var b = s.Begin(Test("Old2"))!;
        s.End(b, new AttemptOutcome { Status = AttemptStatus.Passed });
        var summary = s.Close()!;
        Assert.That(summary.Lost, Is.EqualTo(0));
        Assert.That(server.Final().Count(), Is.EqualTo(2));
        Assert.That(server.Logs, Is.Empty);
        var stdout = server.Final().First(r => r["name"]!.GetValue<string>() == "Old")["stdout"]!.GetValue<string>();
        Assert.That(stdout, Does.Contain("hello").And.Contain("--- log ---").And.Contain("a log line"));
    }

    [Test]
    public void ADownServerSpoolsEverythingAndNeverThrows()
    {
        using var server = new FakeServer { FailWith = 503 };
        var s = Session(server.Url, ("FASTPROOF_FLUSH_TIMEOUT_MS", "1500"));
        for (var i = 0; i < 3; i++)
        {
            var a = s.Begin(Test("Down" + i))!;
            FastProofContext.Attach("log.txt", "text " + i);
            s.End(a, new AttemptOutcome { Status = AttemptStatus.Failed, ErrorMessage = "x" });
        }
        var started = DateTime.UtcNow;
        var summary = s.Close()!;
        Assert.That(DateTime.UtcNow - started, Is.LessThan(TimeSpan.FromSeconds(10)));
        Assert.That(summary.Spooled, Is.EqualTo(3));
        Assert.That(summary.Offline, Is.True);
        var session = Directory.GetDirectories(Path.Combine(_dir, "spool")).Single();
        var meta = JsonNode.Parse(File.ReadAllText(Path.Combine(session, "meta.json")))!;
        Assert.That(meta["v"]!.GetValue<int>(), Is.EqualTo(1));
        Assert.That(meta["ended"]!.GetValue<bool>(), Is.True);
        Assert.That(meta["project"]!.GetValue<string>(), Is.EqualTo("shop"));
        Assert.That(meta.ToJsonString(), Does.Not.Contain("test-token-123456"));
        var lines = File.ReadAllLines(Path.Combine(session, "results.ndjson"));
        Assert.That(lines.Length, Is.EqualTo(3));
        Assert.That(lines[0], Does.Contain("blob:"));
        Assert.That(Directory.GetFiles(Path.Combine(session, "blobs")).Length, Is.EqualTo(3));
    }

    [Test]
    public void ASpoolLeftByAnEarlierRunIsSentByTheNextOne()
    {
        string spoolDir;
        using (var down = new FakeServer { FailWith = 503 })
        {
            var s = Session(down.Url, ("FASTPROOF_FLUSH_TIMEOUT_MS", "1500"), ("FASTPROOF_LAUNCH", "ci-spool-1"));
            var a = s.Begin(Test("Spooled"))!;
            FastProofContext.Attach("evidence.txt", "evidence");
            s.End(a, new AttemptOutcome { Status = AttemptStatus.Failed });
            s.Close();
            spoolDir = Path.Combine(_dir, "spool");
            Assert.That(Directory.GetDirectories(spoolDir), Has.Length.EqualTo(1));
        }
        ReporterSession.Reset();
        using var up = new FakeServer();
        var next = Session(up.Url, ("FASTPROOF_LAUNCH", "ci-spool-1"));
        var b = next.Begin(Test("Fresh"))!;
        next.End(b, new AttemptOutcome { Status = AttemptStatus.Passed });
        next.Close();
        WaitFor(() => up.Final().Count() >= 2);
        Assert.That(up.Final().Select(r => r["name"]!.GetValue<string>()), Is.EquivalentTo(new[] { "Spooled", "Fresh" }));
        var spooled = up.Final().First(r => r["name"]!.GetValue<string>() == "Spooled");
        Assert.That(Encoding.UTF8.GetString(up.Attachments[spooled["attachments"]![0]!["id"]!.GetValue<string>()]), Is.EqualTo("evidence"));
        Assert.That(Directory.Exists(spoolDir) ? Directory.GetDirectories(spoolDir) : Array.Empty<string>(), Is.Empty);
    }

    [Test]
    public void AFlushSendsOnlyBlobsInsideTheSessionThatMatchTheirHash()
    {
        using var server = new FakeServer();
        var secret = Path.Combine(_dir, "secret.txt");
        File.WriteAllText(secret, "outside-the-spool-secret");
        var root = Path.Combine(_dir, "spool");
        var session = Path.Combine(root, "20260101T000000-1-planted");
        var blobs = Path.Combine(session, "blobs");
        Directory.CreateDirectory(blobs);
        var good = Encoding.UTF8.GetBytes("legit evidence");
        var goodSha = Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(good)).ToLowerInvariant();
        File.WriteAllBytes(Path.Combine(blobs, goodSha), good);
        var wrongSha = new string('a', 64);
        File.WriteAllText(Path.Combine(blobs, wrongSha), "outside-the-spool-secret");
        var linkSha = new string('b', 64);
        File.CreateSymbolicLink(Path.Combine(blobs, linkSha), secret);
        File.WriteAllText(Path.Combine(session, "meta.json"), new JsonObject
        {
            ["v"] = 1, ["project"] = "shop", ["pid"] = 1, ["host"] = "elsewhere", ["ended"] = true,
            ["launch"] = new JsonObject { ["externalId"] = "planted", ["name"] = "planted" },
        }.ToJsonString());
        string Line(string name, string sha) => new JsonObject
        {
            ["v"] = 1,
            ["result"] = new JsonObject
            {
                ["id"] = $"dn-000000000000-{name}-0", ["name"] = name, ["status"] = "passed", ["start"] = 1, ["stop"] = 2,
                ["attachments"] = new JsonArray(new JsonObject { ["id"] = "blob:" + sha, ["name"] = "a.txt", ["contentType"] = "text/plain" }),
            },
            ["blobs"] = new JsonArray(new JsonObject { ["sha256"] = sha, ["name"] = "a.txt", ["contentType"] = "text/plain", ["size"] = 10 }),
        }.ToJsonString();
        File.WriteAllLines(Path.Combine(session, "results.ndjson"), new[]
        {
            Line("absolute", secret), Line("dots", "../../../secret.txt"), Line("wrong", wrongSha), Line("link", linkSha), Line("good", goodSha),
        });

        var flushed = SpoolFlusher.Flush(new Api(server.Url, "test-token-123456", "shop", TimeSpan.FromSeconds(5)), root, null, null, "shop");

        Assert.That(flushed.Results, Is.EqualTo(5));
        Assert.That(server.Attachments.Values.Select(v => Encoding.UTF8.GetString(v)), Is.EqualTo(new[] { "legit evidence" }));
        var finals = server.Final().ToDictionary(r => r["name"]!.GetValue<string>());
        Assert.That(finals["good"]["attachments"]!.AsArray().Count, Is.EqualTo(1));
        foreach (var name in new[] { "absolute", "dots", "wrong", "link" })
        {
            Assert.That(finals[name]["attachments"]!.AsArray(), Is.Empty, name);
        }
    }

    [Test]
    public void ConsoleOutputWithoutNewlinesIsCapturedInLinearTimeAndBoundedLines()
    {
        using var server = new FakeServer();
        var s = Session(server.Url);
        var a = s.Begin(Test("Dots"))!;
        var tee = new ConsoleCapture.TeeWriter(TextWriter.Null, false);
        var started = DateTime.UtcNow;
        for (var i = 0; i < 1_000_000; i++)
        {
            tee.Write('.');
        }
        tee.Write("done\n");
        Assert.That(DateTime.UtcNow - started, Is.LessThan(TimeSpan.FromSeconds(10)));
        s.End(a, new AttemptOutcome { Status = AttemptStatus.Passed });
        s.Close();
        Assert.That(server.Final().Single()["stdout"]!.GetValue<string>(), Is.EqualTo(new string('.', 1_000_000) + "done\n"));
        Assert.That(server.Logs.All(l => l["message"]!.GetValue<string>().Length <= Client.MaxLogMessage), Is.True);
    }

    [Test]
    public void AnUnauthorizedTokenOpensTheBreakerAndSpools()
    {
        using var server = new FakeServer { ExpectToken = "another-token" };
        var s = Session(server.Url);
        var a = s.Begin(Test("Denied"))!;
        s.End(a, new AttemptOutcome { Status = AttemptStatus.Passed });
        var summary = s.Close()!;
        Assert.That(summary.Offline, Is.True);
        Assert.That(summary.Spooled, Is.EqualTo(1));
        Assert.That(string.Join("\n", _messages), Does.Not.Contain("test-token-123456"));
    }

    [Test]
    public void ParallelTestsAllReachTheServerOnce()
    {
        using var server = new FakeServer();
        var s = Session(server.Url);
        Parallel.For(0, 60, new ParallelOptions { MaxDegreeOfParallelism = 8 }, i =>
        {
            var a = s.Begin(Test("P" + i))!;
            FastProofContext.Log("line " + i);
            FastProofContext.Step("step", () => { });
            s.End(a, new AttemptOutcome { Status = i % 2 == 0 ? AttemptStatus.Passed : AttemptStatus.Failed });
        });
        s.Close();
        Assert.That(server.Final().Select(r => r["id"]!.GetValue<string>()).Distinct().Count(), Is.EqualTo(60));
        var logs = server.Logs.GroupBy(l => l["resultId"]!.GetValue<string>()).ToList();
        Assert.That(logs.Count, Is.EqualTo(60));
        foreach (var g in logs)
        {
            Assert.That(g.Select(l => l["seq"]!.GetValue<int>()).OrderBy(x => x), Is.EqualTo(Enumerable.Range(0, g.Count())));
        }
    }

    [Test]
    public void TheTestPlanSelectsTests()
    {
        using var server = new FakeServer();
        var plan = Path.Combine(_dir, "plan.json");
        File.WriteAllText(plan, "{\"version\":\"1.0\",\"tests\":[{\"selector\":\"Ns.Type.In\"}]}");
        var s = Session(server.Url, ("FASTPROOF_TESTPLAN", plan));
        var inPlan = Test("In");
        Metadata.AddSelectors(inPlan, "Ns.Type", "In", null);
        var outPlan = Test("Out");
        Metadata.AddSelectors(outPlan, "Ns.Type", "Out", null);
        Assert.That(s.IsSelected(inPlan), Is.True);
        Assert.That(s.IsSelected(outPlan), Is.False);
        s.Close();
    }

    [Test]
    public void AnOfflineRunWritesOnlyTheSpool()
    {
        var s = Session("http://127.0.0.1:9", ("FASTPROOF_OFFLINE", "true"));
        var a = s.Begin(Test("Local"))!;
        s.End(a, new AttemptOutcome { Status = AttemptStatus.Passed });
        var summary = s.Close()!;
        Assert.That(summary.Spooled, Is.EqualTo(1));
        Assert.That(summary.LaunchId, Is.Null);
    }

    [Test]
    public void StepsAndAttachmentsOutsideATestDoNothing()
    {
        Assert.DoesNotThrow(() =>
        {
            FastProofContext.Step("x", () => { });
            FastProofContext.Attach("a", "b");
            FastProofContext.Log("c");
            using var s = FastProofContext.Step("y");
        });
        Assert.That(FastProofContext.Trace.Traceparent, Does.StartWith("00-"));
    }
}
