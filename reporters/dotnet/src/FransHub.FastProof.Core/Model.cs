using System;
using System.Collections.Generic;
using System.Text;
using System.Threading;

namespace FransHub.FastProof;

public enum AttemptStatus
{
    Passed,
    Failed,
    Broken,
    Skipped,
    Unknown,
}

public sealed class LabelItem
{
    public LabelItem(string name, string value)
    {
        Name = name;
        Value = value;
    }

    public string Name { get; }
    public string Value { get; }
}

public sealed class LinkItem
{
    public LinkItem(string url, string? name = null, string type = "link")
    {
        Url = url;
        Name = name;
        Type = type;
    }

    public string Url { get; }
    public string? Name { get; }
    public string Type { get; }
}

public sealed class ParameterItem
{
    public ParameterItem(string name, string? value, string mode = "default")
    {
        Name = name;
        Value = value;
        Mode = mode;
    }

    public string Name { get; }
    public string? Value { get; }
    public string Mode { get; }
}

/// <summary>What an adapter knows about a test when an attempt begins.</summary>
public sealed class TestDescriptor
{
    public string Name { get; set; } = string.Empty;
    public string FullName { get; set; } = string.Empty;
    public string Framework { get; set; } = "dotnet";
    public List<LabelItem> Labels { get; } = new();
    public List<LinkItem> Links { get; } = new();
    public List<ParameterItem> Parameters { get; } = new();
    public string? TestCaseId { get; set; }
    public string? Description { get; set; }
    public string? SourceFile { get; set; }
    public int SourceLine { get; set; }

    /// <summary>Selectors a test plan may use for this test (full name with and without parameters, Allure style).</summary>
    public List<string> PlanSelectors { get; } = new();

    /// <summary>Ids a test plan may use (explicit FastProof id, <c>allure_id</c>, <c>as_id</c>).</summary>
    public List<string> PlanIds { get; } = new();

    internal string Key()
    {
        var sb = new StringBuilder(FullName);
        foreach (var p in Parameters)
        {
            sb.Append('\0').Append(p.Name).Append('=').Append(p.Value);
        }
        return sb.ToString();
    }
}

/// <summary>The outcome of one attempt, reported by the adapter when the attempt ends.</summary>
public sealed class AttemptOutcome
{
    public AttemptStatus Status { get; set; } = AttemptStatus.Unknown;
    public string? ErrorMessage { get; set; }
    public string? ErrorTrace { get; set; }
    public string? ExtraStdout { get; set; }
    public string? ExtraStderr { get; set; }
}

internal sealed class StepNode
{
    public string Name = string.Empty;
    public long Start;
    public long Stop;
    public AttemptStatus Status = AttemptStatus.Passed;
    public string? Error;
    public List<StepNode> Children = new();
    public StepNode? Parent;
}

internal sealed class BoundedText
{
    private readonly StringBuilder _sb = new();
    private readonly int _limit;
    private bool _truncated;

    public BoundedText(int limit) => _limit = limit;

    public void Append(string s)
    {
        lock (_sb)
        {
            if (_sb.Length + s.Length > _limit)
            {
                var room = Math.Max(0, _limit - _sb.Length);
                _sb.Append(s, 0, Math.Min(room, s.Length));
                _truncated = true;
            }
            else
            {
                _sb.Append(s);
            }
        }
    }

    public override string ToString()
    {
        lock (_sb)
        {
            return _truncated ? _sb + "\n…[truncated]" : _sb.ToString();
        }
    }

    public bool IsEmpty
    {
        get
        {
            lock (_sb)
            {
                return _sb.Length == 0;
            }
        }
    }
}

/// <summary>One attempt of one test: its client id, trace context, steps, files and output.</summary>
public sealed class Attempt
{
    private static readonly AsyncLocal<Attempt?> CurrentValue = new();

    internal Attempt(string id, int number, TestDescriptor descriptor, TraceContext trace, long start)
    {
        Id = id;
        Number = number;
        Descriptor = descriptor;
        Trace = trace;
        Start = start;
    }

    /// <summary>Lets an adapter find the running attempt when the async flow of the test does not carry it.</summary>
    public static Func<Attempt?>? Resolver { get; set; }

    public static Attempt? Current
    {
        get => CurrentValue.Value ?? Resolver?.Invoke();
        internal set => CurrentValue.Value = value;
    }

    public string Id { get; }
    public int Number { get; }
    public TraceContext Trace { get; }
    public TestDescriptor Descriptor { get; }
    internal long Start { get; }
    internal List<StepNode> Steps { get; } = new();
    internal StepNode? OpenStep;
    internal List<FileRef> Files { get; } = new();
    internal BoundedText Stdout { get; } = new(1024 * 1024);
    internal BoundedText Stderr { get; } = new(1024 * 1024);
    internal BoundedText LogText { get; } = new(1024 * 1024);
    internal object Lock { get; } = new();
    internal bool Ended;
    internal StringBuilder PartialOut { get; } = new();
    internal StringBuilder PartialErr { get; } = new();
}
