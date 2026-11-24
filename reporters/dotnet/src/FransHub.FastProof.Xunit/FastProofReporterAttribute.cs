using System;
using System.Collections.Concurrent;
using System.Linq;
using System.Reflection;
using System.Text;
using System.Threading;
using FransHub.FastProof;
using Xunit;
using Xunit.Sdk;
using Xunit.v3;

namespace FransHub.FastProof.Xunit;

/// <summary>Reports every test of the assembly to FastProof. Add <c>[assembly: FastProofReporter]</c> once; the reporter
/// stays off without <c>FASTPROOF_URL</c>. Theory rows are separate results; output written to
/// <c>ITestOutputHelper</c> streams live (and the console too with <c>[assembly: CaptureConsole]</c>).</summary>
[AttributeUsage(AttributeTargets.Assembly | AttributeTargets.Class | AttributeTargets.Method, AllowMultiple = false)]
public sealed class FastProofReporterAttribute : BeforeAfterTestAttribute
{
    private static readonly ConcurrentDictionary<string, Running> Active = new();
    private static int s_resolver;

    public FastProofReporterAttribute()
    {
    }

    /// <summary>Overrides <c>FASTPROOF_PROJECT</c>.</summary>
    public string? Project { get; set; }

    /// <summary>Overrides <c>FASTPROOF_URL</c>.</summary>
    public string? Url { get; set; }

    private sealed class Running
    {
        public Attempt Attempt = null!;
        public Timer? Poller;
        public int Seen;
        public ITestOutputHelper? Output;
        public readonly object Lock = new();
    }

    public override void Before(MethodInfo methodUnderTest, IXunitTest test)
    {
        try
        {
            var session = ReporterSession.Start("xunit", new FastProofOptions { Project = Project, Url = Url }, captureConsole: false);
            if (session == null)
            {
                return;
            }
            if (Interlocked.Exchange(ref s_resolver, 1) == 0)
            {
                Attempt.Resolver = () => TestContext.Current.Test is { } t && Active.TryGetValue(t.UniqueID, out var r) ? r.Attempt : null;
            }
            var d = Describe(methodUnderTest, test);
            if (!session.IsSelected(d))
            {
                throw new SkipException(DynamicSkipToken.Value + "not in the FastProof test plan");
            }
            var attempt = session.Begin(d);
            if (attempt == null)
            {
                return;
            }
            var run = new Running { Attempt = attempt, Output = TestContext.Current.TestOutputHelper };
            Active[test.UniqueID] = run;
            run.Poller = new Timer(_ => Drain(session, run, false), null, 200, 200);
        }
        catch (SkipException)
        {
            throw;
        }
        catch (Exception e)
        {
            Console.Error.WriteLine($"[fastproof] reporting error (ignored): {e.Message}");
        }
    }

    public override void After(MethodInfo methodUnderTest, IXunitTest test)
    {
        try
        {
            if (!Active.TryRemove(test.UniqueID, out var run))
            {
                return;
            }
            run.Poller?.Dispose();
            var session = ReporterSession.Current;
            if (session == null)
            {
                return;
            }
            Drain(session, run, true);
            session.End(run.Attempt, Outcome(TestContext.Current.TestState));
        }
        catch (Exception e)
        {
            Console.Error.WriteLine($"[fastproof] reporting error (ignored): {e.Message}");
        }
    }

    private sealed class SkipException : Exception
    {
        public SkipException(string message) : base(message)
        {
        }
    }

    private static void Drain(ReporterSession session, Running run, bool final)
    {
        try
        {
            lock (run.Lock)
            {
                var all = run.Output?.Output;
                if (string.IsNullOrEmpty(all) || all.Length <= run.Seen)
                {
                    return;
                }
                var end = final ? all.Length : all.LastIndexOf('\n') + 1;
                if (end <= run.Seen)
                {
                    return;
                }
                var chunk = all.Substring(run.Seen, end - run.Seen);
                run.Seen = end;
                session.Output(run.Attempt, chunk);
            }
        }
        catch (Exception)
        {
        }
    }

    internal static AttemptOutcome Outcome(TestResultState? state)
    {
        var o = new AttemptOutcome { Status = AttemptStatus.Unknown };
        if (state == null)
        {
            return o;
        }
        switch (state.Result)
        {
            case TestResult.Passed:
                o.Status = AttemptStatus.Passed;
                break;
            case TestResult.Skipped:
            case TestResult.NotRun:
                o.Status = AttemptStatus.Skipped;
                break;
            case TestResult.Failed:
                o.Status = IsAssertion(state.ExceptionTypes?.FirstOrDefault()) ? AttemptStatus.Failed : AttemptStatus.Broken;
                break;
        }
        var msgs = state.ExceptionMessages;
        if (msgs is { Length: > 0 })
        {
            o.ErrorMessage = string.Join("\n", msgs.Where(m => m != null));
            if (o.Status == AttemptStatus.Broken && state.ExceptionTypes?.FirstOrDefault() is { } type)
            {
                o.ErrorMessage = $"{type}: {o.ErrorMessage}";
            }
            o.ErrorTrace = state.ExceptionStackTraces is { Length: > 0 } ? string.Join("\n---\n", state.ExceptionStackTraces.Where(t => t != null)) : null;
            if (o.ErrorMessage.StartsWith(DynamicSkipToken.Value, StringComparison.Ordinal))
            {
                o.Status = AttemptStatus.Skipped;
                o.ErrorMessage = o.ErrorMessage.Substring(DynamicSkipToken.Value.Length);
                o.ErrorTrace = null;
            }
        }
        return o;
    }

    private static bool IsAssertion(string? type) =>
        type != null && (type.StartsWith("Xunit.Sdk.", StringComparison.Ordinal) && type.EndsWith("Exception", StringComparison.Ordinal) && !type.Contains("TestTimeout", StringComparison.Ordinal)
            || type.EndsWith("AssertionException", StringComparison.Ordinal));

    internal static TestDescriptor Describe(MethodInfo method, IXunitTest test)
    {
        var typeName = method.DeclaringType?.FullName ?? string.Empty;
        var d = new TestDescriptor { Framework = "xunit", FullName = $"{typeName}.{method.Name}" };
        var display = test.TestDisplayName;
        d.Name = display.StartsWith(typeName + ".", StringComparison.Ordinal) ? display.Substring(typeName.Length + 1) : display;
        Metadata.Apply(d, method);
        Metadata.AddParameters(d, method, test.TestMethodArguments);
        Metadata.AddSelectors(d, typeName.Replace('+', '.'), method.Name, display);

        var type = method.DeclaringType;
        var ns = type?.Namespace ?? string.Empty;
        var classes = (type?.FullName ?? string.Empty);
        if (ns.Length > 0 && classes.StartsWith(ns + ".", StringComparison.Ordinal))
        {
            classes = classes.Substring(ns.Length + 1);
        }
        var parts = classes.Split('+');
        if (ns.Length > 0)
        {
            d.Labels.Add(new LabelItem("parentSuite", ns));
        }
        if (parts[0].Length > 0)
        {
            d.Labels.Add(new LabelItem("suite", parts[0]));
        }
        if (parts.Length > 1)
        {
            d.Labels.Add(new LabelItem("subSuite", string.Join(" › ", parts.Skip(1))));
        }
        d.Labels.Add(new LabelItem("thread", Environment.CurrentManagedThreadId.ToString(System.Globalization.CultureInfo.InvariantCulture)));
        foreach (var kv in test.Traits)
        {
            foreach (var v in kv.Value)
            {
                if (kv.Key.Equals("Category", StringComparison.OrdinalIgnoreCase))
                {
                    d.Labels.Add(new LabelItem("tag", v));
                }
                else if (kv.Key is "fastproof.id")
                {
                    d.TestCaseId = v;
                    d.PlanIds.Add(v);
                }
                else if (kv.Key is "allure_id" or "as_id")
                {
                    d.PlanIds.Add(v);
                }
                else
                {
                    d.Labels.Add(new LabelItem(kv.Key, v));
                }
            }
        }
        return d;
    }
}
