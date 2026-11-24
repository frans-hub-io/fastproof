using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using FransHub.FastProof;
using NUnit.Framework;
using NUnit.Framework.Interfaces;

namespace FransHub.FastProof.NUnit;

/// <summary>Reports every test of the assembly to FastProof. Add <c>[assembly: FastProofReporter]</c> once; the reporter
/// stays off without <c>FASTPROOF_URL</c>. Each run of a test (a <c>[Retry]</c> or <c>[Repeat]</c> pass) is one attempt;
/// <c>[TestCase]</c> and <c>[TestCaseSource]</c> rows are separate results.</summary>
[AttributeUsage(AttributeTargets.Assembly)]
public sealed class FastProofReporterAttribute : NUnitAttribute, ITestAction
{
    private static readonly System.Threading.AsyncLocal<Attempt?> Running = new();

    public FastProofReporterAttribute()
    {
    }

    public FastProofReporterAttribute(string project)
    {
        Project = project;
    }

    /// <summary>Overrides <c>FASTPROOF_PROJECT</c>.</summary>
    public string? Project { get; set; }

    /// <summary>Overrides <c>FASTPROOF_URL</c>.</summary>
    public string? Url { get; set; }

    public ActionTargets Targets => ActionTargets.Test | ActionTargets.Suite;

    public void BeforeTest(ITest test)
    {
        try
        {
            if (test.IsSuite)
            {
                if (test.TestType == "Assembly")
                {
                    ReportNotRunnable(Session(), test);
                }
                return;
            }
            var session = Session();
            if (session == null)
            {
                return;
            }
            var d = Describe(test);
            if (!session.IsSelected(d))
            {
                Running.Value = null;
                Assert.Ignore("not in the FastProof test plan");
                return;
            }
            Running.Value = session.Begin(d);
        }
        catch (Exception e) when (e is not ResultStateException)
        {
            Console.Error.WriteLine($"[fastproof] reporting error (ignored): {e.Message}");
        }
    }

    public void AfterTest(ITest test)
    {
        try
        {
            var session = ReporterSession.Current;
            if (test.IsSuite)
            {
                if (test.TestType == "Assembly")
                {
                    session?.Close();
                }
                return;
            }
            var attempt = Running.Value;
            Running.Value = null;
            if (attempt == null || session == null)
            {
                return;
            }
            session.End(attempt, Outcome(TestContext.CurrentContext.Result));
        }
        catch (Exception e)
        {
            Console.Error.WriteLine($"[fastproof] reporting error (ignored): {e.Message}");
        }
    }

    private static void ReportNotRunnable(ReporterSession? session, ITest suite)
    {
        if (session == null)
        {
            return;
        }
        foreach (var t in suite.Tests)
        {
            if (t.IsSuite)
            {
                ReportNotRunnable(session, t);
                continue;
            }
            if (t.RunState is not (RunState.Ignored or RunState.Skipped or RunState.NotRunnable))
            {
                continue;
            }
            var d = Describe(t);
            if (!session.IsSelected(d))
            {
                continue;
            }
            var attempt = session.Begin(d);
            var reason = t.Properties.Get("_SKIPREASON")?.ToString();
            session.End(attempt, new AttemptOutcome
            {
                Status = t.RunState == RunState.NotRunnable ? AttemptStatus.Broken : AttemptStatus.Skipped,
                ErrorMessage = reason,
            });
        }
    }

    private ReporterSession? Session()
    {
        var o = new FastProofOptions { Project = Project, Url = Url };
        return ReporterSession.Start("nunit", o);
    }

    internal static AttemptOutcome Outcome(TestContext.ResultAdapter r)
    {
        var o = new AttemptOutcome { Status = Map(r.Outcome) };
        if (o.Status != AttemptStatus.Passed)
        {
            o.ErrorMessage = string.IsNullOrEmpty(r.Message) ? null : r.Message;
            o.ErrorTrace = string.IsNullOrEmpty(r.StackTrace) ? null : r.StackTrace;
        }
        return o;
    }

    internal static AttemptStatus Map(ResultState s)
    {
        switch (s.Status)
        {
            case TestStatus.Passed:
                return AttemptStatus.Passed;
            case TestStatus.Skipped:
                return AttemptStatus.Skipped;
            case TestStatus.Failed:
                return s.Label is "Error" or "Invalid" or "Cancelled" || s.Site is FailureSite.SetUp or FailureSite.TearDown or FailureSite.Parent
                    ? AttemptStatus.Broken
                    : AttemptStatus.Failed;
            case TestStatus.Warning:
                return AttemptStatus.Passed;
            default:
                return AttemptStatus.Unknown;
        }
    }

    internal static TestDescriptor Describe(ITest test)
    {
        var method = test.Method?.MethodInfo;
        var d = new TestDescriptor { Framework = "nunit", Name = test.Name };
        var className = test.ClassName ?? string.Empty;
        var methodName = test.MethodName ?? test.Name;
        d.FullName = className.Length > 0 ? $"{className}.{methodName}" : methodName;
        Metadata.Apply(d, method);
        Metadata.AddParameters(d, method, test.Arguments);
        Metadata.AddSelectors(d, className.Replace('+', '.'), methodName, test.FullName);

        var plus = className.Replace('+', '.');
        var lastDot = plus.LastIndexOf('.');
        var ns = test.TypeInfo?.Namespace ?? (lastDot > 0 ? plus.Substring(0, lastDot) : string.Empty);
        var typeParts = ns.Length > 0 && className.StartsWith(ns + ".", StringComparison.Ordinal) ? className.Substring(ns.Length + 1) : className;
        var classes = typeParts.Split('+');
        if (ns.Length > 0)
        {
            d.Labels.Add(new LabelItem("parentSuite", ns));
        }
        if (classes.Length > 0 && classes[0].Length > 0)
        {
            d.Labels.Add(new LabelItem("suite", classes[0]));
        }
        if (classes.Length > 1)
        {
            d.Labels.Add(new LabelItem("subSuite", string.Join(" › ", classes.Skip(1))));
        }
        d.Labels.Add(new LabelItem("thread", System.Threading.Thread.CurrentThread.ManagedThreadId.ToString(System.Globalization.CultureInfo.InvariantCulture)));

        var chain = new List<ITest>();
        for (ITest? t = test; t != null && t.TestType != "Assembly"; t = t.Parent)
        {
            chain.Insert(0, t);
        }
        var tags = new HashSet<string>();
        var props = new List<(string Key, object? Value)>();
        foreach (var t in chain)
        {
            foreach (var key in t.Properties.Keys)
            {
                foreach (var value in t.Properties[key])
                {
                    props.Add((key, value));
                }
            }
        }
        foreach (var (key, value) in props)
        {
            var v = value?.ToString() ?? string.Empty;
            switch (key)
            {
                case "Category":
                    if (tags.Add(v))
                    {
                        d.Labels.Add(new LabelItem("tag", v));
                    }
                    break;
                case "Description":
                    d.Description = v;
                    break;
                case "fastproof.id":
                    d.TestCaseId = v;
                    d.PlanIds.Add(v);
                    break;
                case "allure_id":
                case "as_id":
                    d.PlanIds.Add(v);
                    break;
                case "_SKIPREASON":
                case "_PENDINGREASON":
                case "_JOINTYPE":
                case "_APPDOMAIN":
                    break;
                default:
                    if (!key.StartsWith('_'))
                    {
                        d.Labels.Add(new LabelItem(key, v));
                    }
                    break;
            }
        }
        return d;
    }
}
