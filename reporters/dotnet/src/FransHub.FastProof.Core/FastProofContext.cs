using System;
using System.Collections.Generic;
using System.IO;
using System.Threading.Tasks;

namespace FransHub.FastProof;

/// <summary>Calls tests make: steps, attachments, log lines and the trace context of the running attempt.
/// All of them do nothing when the reporter is off or no attempt is running.</summary>
public static class FastProofContext
{
    /// <summary>Trace context of the current attempt (for HTTP headers and child processes); a fresh, unreported one outside a test.</summary>
    public static TraceContext Trace => Attempt.Current?.Trace ?? TraceContext.Create(string.Empty, string.Empty, 0);

    /// <summary>True while a test of the current async flow is being reported.</summary>
    public static bool IsActive => Attempt.Current != null;

    public static IDisposable Step(string name) => new StepScope(name);

    public static void Step(string name, Action body)
    {
        using var s = new StepScope(name);
        try
        {
            body();
        }
        catch (Exception e)
        {
            s.Fail(e);
            throw;
        }
    }

    public static T Step<T>(string name, Func<T> body)
    {
        using var s = new StepScope(name);
        try
        {
            return body();
        }
        catch (Exception e)
        {
            s.Fail(e);
            throw;
        }
    }

    public static async Task StepAsync(string name, Func<Task> body)
    {
        using var s = new StepScope(name);
        try
        {
            await body().ConfigureAwait(false);
        }
        catch (Exception e)
        {
            s.Fail(e);
            throw;
        }
    }

    public static async Task<T> StepAsync<T>(string name, Func<Task<T>> body)
    {
        using var s = new StepScope(name);
        try
        {
            return await body().ConfigureAwait(false);
        }
        catch (Exception e)
        {
            s.Fail(e);
            throw;
        }
    }

    public static void Attach(string name, byte[] body, string contentType = "application/octet-stream")
    {
        if (Attempt.Current is { Ended: false } a)
        {
            lock (a.Lock)
            {
                a.Files.Add(new FileRef { Name = name, ContentType = contentType, Body = body });
            }
        }
    }

    public static void Attach(string name, string text, string contentType = "text/plain") =>
        Attach(name, new System.Text.UTF8Encoding(false).GetBytes(text), contentType);

    /// <summary>Attaches a file; it is hashed and uploaded in the background, so it must stay until the test ends.</summary>
    public static void AttachFile(string path, string? name = null, string? contentType = null)
    {
        if (Attempt.Current is { Ended: false } a)
        {
            lock (a.Lock)
            {
                a.Files.Add(new FileRef { Name = name ?? System.IO.Path.GetFileName(path), ContentType = contentType ?? ContentTypes.For(path), Path = path });
            }
        }
    }

    /// <summary>A live log line of the current attempt (level: trace, debug, info, warn, error).</summary>
    public static void Log(string message, string level = "info")
    {
        if (Attempt.Current is { Ended: false } a && ReporterSession.Current is { } s)
        {
            s.LogLine(a, level, "log", message);
        }
    }

    private sealed class StepScope : IDisposable
    {
        private readonly Attempt? _attempt;
        private readonly StepNode? _node;
        private readonly string _path = string.Empty;
        private bool _done;

        public StepScope(string name)
        {
            _attempt = Attempt.Current;
            if (_attempt == null || _attempt.Ended)
            {
                return;
            }
            lock (_attempt.Lock)
            {
                var parent = _attempt.OpenStep;
                _node = new StepNode { Name = name, Start = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(), Parent = parent };
                (parent?.Children ?? _attempt.Steps).Add(_node);
                _attempt.OpenStep = _node;
                _path = Path(_node);
            }
            ReporterSession.Current?.LogLine(_attempt, "info", "step", $"▶ {name}", _path);
        }

        public void Fail(Exception e)
        {
            if (_node != null)
            {
                _node.Status = AttemptStatus.Failed;
                _node.Error = e.Message;
            }
        }

        public void Dispose()
        {
            if (_done || _node == null || _attempt == null)
            {
                return;
            }
            _done = true;
            lock (_attempt.Lock)
            {
                _node.Stop = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
                if (_attempt.OpenStep == _node)
                {
                    _attempt.OpenStep = _node.Parent;
                }
            }
            var ms = _node.Stop - _node.Start;
            var mark = _node.Status == AttemptStatus.Passed ? "✔" : "✘";
            ReporterSession.Current?.LogLine(_attempt, _node.Status == AttemptStatus.Passed ? "info" : "error", "step", $"{mark} {_node.Name} ({ms} ms)", _path);
        }

        private static string Path(StepNode n)
        {
            var parts = new List<string>();
            for (var s = n; s != null; s = s.Parent)
            {
                parts.Add(s.Name);
            }
            parts.Reverse();
            return string.Join(" › ", parts);
        }
    }

}

internal static class ContentTypes
{
    public static string For(string path) => System.IO.Path.GetExtension(path).ToLowerInvariant() switch
    {
        ".png" => "image/png",
        ".jpg" or ".jpeg" => "image/jpeg",
        ".gif" => "image/gif",
        ".webp" => "image/webp",
        ".svg" => "image/svg+xml",
        ".webm" => "video/webm",
        ".mp4" => "video/mp4",
        ".txt" or ".log" => "text/plain",
        ".json" => "application/json",
        ".xml" => "application/xml",
        ".html" or ".htm" => "text/html",
        ".csv" => "text/csv",
        ".zip" => "application/zip",
        ".pdf" => "application/pdf",
        _ => "application/octet-stream",
    };
}

/// <summary>An explicit, stable FastProof test case id (also what test plans select by).</summary>
[AttributeUsage(AttributeTargets.Method | AttributeTargets.Class, AllowMultiple = false)]
public sealed class FastProofIdAttribute : Attribute
{
    public FastProofIdAttribute(string id) => Id = id;

    public string Id { get; }
}

/// <summary>A FastProof label on a test or fixture.</summary>
[AttributeUsage(AttributeTargets.Method | AttributeTargets.Class | AttributeTargets.Assembly, AllowMultiple = true)]
public sealed class FastProofLabelAttribute : Attribute
{
    public FastProofLabelAttribute(string name, string value)
    {
        Name = name;
        Value = value;
    }

    public string Name { get; }
    public string Value { get; }
}

/// <summary>A link (issue, tms or any URL) on a test or fixture.</summary>
[AttributeUsage(AttributeTargets.Method | AttributeTargets.Class, AllowMultiple = true)]
public sealed class FastProofLinkAttribute : Attribute
{
    public FastProofLinkAttribute(string url) => Url = url;

    public string Url { get; }
    public string? Name { get; set; }
    public string Type { get; set; } = "link";
}
