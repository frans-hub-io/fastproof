using System;
using System.Collections.Generic;
using System.Linq;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;
using System.Threading;

namespace FransHub.FastProof;

/// <summary>The reporter of one test process. Adapters call <see cref="Begin"/> when an attempt starts and
/// <see cref="End"/> when it ends; everything else (batching, retries, spool, masking) happens here.</summary>
public sealed class ReporterSession
{
    private static readonly object Gate = new();
    private static ReporterSession? s_current;
    private static bool s_created;

    private readonly Config _cfg;
    private readonly Client? _client;
    private readonly TestPlan? _plan;
    private readonly string _runId = TraceContext.RandomHex(6);
    private readonly Dictionary<string, int> _attempts = new();
    private readonly string _framework;
    private readonly object _lock = new();
    private RunSummary? _summary;
    private bool _closed;

    private ReporterSession(Config cfg, string framework, Action<string>? output, bool captureConsole)
    {
        _cfg = cfg;
        _framework = framework;
        if (!cfg.Enabled)
        {
            return;
        }
        var client = new Client(cfg, output);
        _client = client;
        if (cfg.TestPlanPath != null)
        {
            try
            {
                _plan = TestPlan.Load(cfg.TestPlanPath);
            }
            catch (Exception e)
            {
                client.WarnOnce("plan", $"the test plan {cfg.TestPlanPath} is ignored: {e.Message}");
            }
        }
        client.Start();
        if (captureConsole)
        {
            ConsoleCapture.Install();
        }
    }

    /// <summary>The session of this process, created on first use from the environment; <c>null</c> when the reporter is off.</summary>
    public static ReporterSession? Current
    {
        get
        {
            lock (Gate)
            {
                return s_current;
            }
        }
    }

    /// <summary>Starts (once per process) and returns the session; <c>null</c> when reporting is off.</summary>
    public static ReporterSession? Start(string framework, FastProofOptions? options = null, Action<string>? output = null, bool captureConsole = true)
    {
        lock (Gate)
        {
            if (s_created)
            {
                return s_current;
            }
            s_created = true;
            try
            {
                var cfg = Config.Resolve(options);
                var session = new ReporterSession(cfg, framework, output, captureConsole);
                if (!cfg.Enabled)
                {
                    if (cfg.Reason.Length > 0 && cfg.Reason != "disabled" && !cfg.Quiet)
                    {
                        (output ?? Console.Error.WriteLine)($"[fastproof] reporting is off: {cfg.Reason}");
                    }
                    return null;
                }
                s_current = session;
                AppDomain.CurrentDomain.ProcessExit += (_, _) => session.Close();
                return session;
            }
            catch (Exception e)
            {
                (output ?? Console.Error.WriteLine)($"[fastproof] reporting is off: {e.Message}");
                return null;
            }
        }
    }

    internal static ReporterSession? Create(Config cfg, Action<string>? output = null)
    {
        lock (Gate)
        {
            s_created = true;
            s_current = cfg.Enabled ? new ReporterSession(cfg, "test", output, false) : null;
            return s_current;
        }
    }

    internal static void Reset()
    {
        lock (Gate)
        {
            s_created = false;
            s_current = null;
        }
    }

    public Config Config => _cfg;

    public Masker Masker => _client?.Masker ?? Masker.None;

    private string? M(string? s) => _client != null ? _client.Masker.Apply(s) : s;

    /// <summary>False when a test plan is configured and does not select the test (it is not run and not reported).</summary>
    public bool IsSelected(TestDescriptor d) => _plan == null || _plan.Matches(d);

    public Attempt? Begin(TestDescriptor d)
    {
        if (_client == null || _closed)
        {
            return null;
        }
        try
        {
            var key = d.Key();
            int number;
            lock (_lock)
            {
                _attempts.TryGetValue(key, out number);
                _attempts[key] = number + 1;
            }
            var hash = Convert.ToHexString(SHA1.HashData(Encoding.UTF8.GetBytes(key))).ToLowerInvariant().Substring(0, 16);
            var id = $"dn-{_runId}-{hash}-{number}";
            var trace = TraceContext.Create(_cfg.ExternalLaunchId, d.FullName, number);
            var attempt = new Attempt(id, number, d, trace, DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());
            Attempt.Current = attempt;
            var running = Identity(attempt);
            running["start"] = attempt.Start;
            _client.OpenRunning(running);
            return attempt;
        }
        catch (Exception e)
        {
            _client.WarnOnce("begin", $"reporting error (ignored): {e.Message}");
            return null;
        }
    }

    private JsonObject Identity(Attempt a)
    {
        var d = a.Descriptor;
        var labels = new JsonArray { new JsonObject { ["name"] = "framework", ["value"] = d.Framework } };
        foreach (var l in d.Labels)
        {
            labels.Add(new JsonObject { ["name"] = l.Name, ["value"] = M(l.Value) });
        }
        var links = new JsonArray();
        foreach (var l in d.Links)
        {
            var o = new JsonObject { ["url"] = l.Url, ["type"] = l.Type };
            if (l.Name != null)
            {
                o["name"] = M(l.Name);
            }
            links.Add(o);
        }
        var ps = new JsonArray();
        foreach (var p in d.Parameters)
        {
            var o = new JsonObject { ["name"] = M(p.Name), ["value"] = M(p.Value) };
            if (p.Mode != "default")
            {
                o["mode"] = p.Mode;
            }
            ps.Add(o);
        }
        var r = new JsonObject
        {
            ["id"] = a.Id,
            ["name"] = M(d.Name),
            ["fullName"] = M(d.FullName),
            ["retry"] = a.Number,
            ["labels"] = labels,
            ["links"] = links,
            ["parameters"] = ps,
            ["traceparent"] = a.Trace.Traceparent,
        };
        if (d.TestCaseId != null)
        {
            r["testCaseId"] = M(d.TestCaseId);
        }
        if (d.Description != null)
        {
            r["description"] = M(d.Description);
        }
        if (d.SourceFile != null)
        {
            r["source"] = new JsonObject { ["file"] = d.SourceFile, ["line"] = Math.Max(0, d.SourceLine) };
        }
        return r;
    }

    internal static string StatusName(AttemptStatus s) => s switch
    {
        AttemptStatus.Passed => "passed",
        AttemptStatus.Failed => "failed",
        AttemptStatus.Broken => "broken",
        AttemptStatus.Skipped => "skipped",
        _ => "unknown",
    };

    private JsonObject StepJson(StepNode s)
    {
        var o = new JsonObject
        {
            ["name"] = M(s.Name),
            ["status"] = StatusName(s.Status),
            ["start"] = s.Start,
            ["stop"] = s.Stop,
            ["durationMs"] = Math.Max(0, s.Stop - s.Start),
        };
        if (s.Error != null)
        {
            o["error"] = new JsonObject { ["message"] = M(s.Error) };
        }
        if (s.Children.Count > 0)
        {
            var arr = new JsonArray();
            foreach (var c in s.Children)
            {
                arr.Add(StepJson(c));
            }
            o["steps"] = arr;
        }
        return o;
    }

    public void End(Attempt? attempt, AttemptOutcome outcome)
    {
        if (attempt == null || _client == null)
        {
            return;
        }
        try
        {
            ConsoleCapture.FlushPartial(attempt);
            lock (attempt.Lock)
            {
                if (attempt.Ended)
                {
                    return;
                }
                attempt.Ended = true;
                CloseOpenSteps(attempt);
            }
            if (Attempt.Current == attempt)
            {
                Attempt.Current = null;
            }
            var stop = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
            var r = Identity(attempt);
            r["status"] = StatusName(outcome.Status);
            r["start"] = attempt.Start;
            r["stop"] = stop;
            r["durationMs"] = Math.Max(0, stop - attempt.Start);
            if (outcome.ErrorMessage != null || outcome.ErrorTrace != null)
            {
                var e = new JsonObject();
                if (outcome.ErrorMessage != null)
                {
                    e["message"] = Client.Cut(M(outcome.ErrorMessage)!, 64 * 1024);
                }
                if (outcome.ErrorTrace != null)
                {
                    e["trace"] = Client.Cut(M(outcome.ErrorTrace)!, 256 * 1024);
                }
                r["error"] = e;
            }
            if (attempt.Steps.Count > 0)
            {
                var steps = new JsonArray();
                foreach (var s in attempt.Steps)
                {
                    steps.Add(StepJson(s));
                }
                r["steps"] = steps;
            }
            var stdout = attempt.Stdout.ToString() + (outcome.ExtraStdout ?? string.Empty);
            var stderr = attempt.Stderr.ToString() + (outcome.ExtraStderr ?? string.Empty);
            if (stdout.Length > 0)
            {
                r["stdout"] = M(stdout);
            }
            if (stderr.Length > 0)
            {
                r["stderr"] = M(stderr);
            }
            var files = attempt.Files.Select(f => new FileRef { Name = M(f.Name) ?? f.Name, ContentType = f.ContentType, Path = f.Path, Body = f.Body }).ToList();
            var fallback = attempt.LogText.IsEmpty ? null : attempt.LogText.ToString();
            _client.Finish(r, files, outcome.Status is AttemptStatus.Passed or AttemptStatus.Skipped, fallback);
        }
        catch (Exception e)
        {
            _client.WarnOnce("end", $"reporting error (ignored): {e.Message}");
        }
    }

    private static void CloseOpenSteps(Attempt a)
    {
        var now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        for (var s = a.OpenStep; s != null; s = s.Parent)
        {
            s.Stop = now;
            s.Status = AttemptStatus.Broken;
        }
        a.OpenStep = null;
    }

    /// <summary>Adds output text (complete lines) captured by an adapter to the attempt and streams it live.</summary>
    public void Output(Attempt attempt, string text, bool error = false)
    {
        if (text.Length == 0)
        {
            return;
        }
        (error ? attempt.Stderr : attempt.Stdout).Append(text);
        foreach (var line in text.TrimEnd('\n').Split('\n'))
        {
            LogLine(attempt, error ? "warn" : "info", error ? "stderr" : "stdout", line.TrimEnd('\r'));
        }
    }

    internal void LogLine(Attempt a, string level, string stream, string message, string? step = null)
    {
        if (_client == null)
        {
            return;
        }
        _client.Log(a.Id, a.Number, level, stream, message, step);
        if (stream is "log" or "step")
        {
            a.LogText.Append(_client.Masker.Text(message) + "\n");
        }
    }

    /// <summary>Waits for uploads, spools what could not be sent, finishes an own launch and prints one summary line.</summary>
    public RunSummary? Close(bool interrupted = false)
    {
        lock (_lock)
        {
            if (_closed || _client == null)
            {
                return _summary;
            }
            _closed = true;
        }
        try
        {
            var s = _client.Close(interrupted ? "interrupted" : "finished");
            _summary = s;
            var where = s.LaunchNumber != null ? $"launch #{s.LaunchNumber}" : "launch";
            if (s.Spooled > 0)
            {
                var reason = s.OfflineReason.Length > 0 ? $" ({s.OfflineReason})" : string.Empty;
                _client.Info($"{s.Sent} results sent, {s.Spooled} saved to {s.SpoolDir}{reason}");
                _client.Info($"the next run sends them, or now: python -m pytest_fastproof flush --spool {_cfg.SpoolDir} (with FASTPROOF_URL, FASTPROOF_TOKEN and FASTPROOF_PROJECT set)");
            }
            else
            {
                _client.Info($"{where}: {s.Sent} results, {s.Attachments} attachments sent");
            }
            if (s.Lost > 0)
            {
                _client.Info($"{s.Lost} results could not be reported");
            }
            return s;
        }
        catch (Exception)
        {
            return _summary;
        }
    }
}
