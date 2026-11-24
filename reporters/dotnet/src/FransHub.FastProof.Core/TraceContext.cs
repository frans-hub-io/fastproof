using System;
using System.Collections.Generic;
using System.Security.Cryptography;
using System.Text.RegularExpressions;

namespace FransHub.FastProof;

/// <summary>W3C trace context of one attempt: <c>traceparent</c> and <c>baggage</c>.</summary>
public sealed class TraceContext
{
    private static readonly Regex TraceparentPattern = new("^00-([0-9a-f]{32})-([0-9a-f]{16})-[0-9a-f]{2}$", RegexOptions.CultureInvariant);

    public TraceContext(string traceId, string spanId, string baggage)
    {
        TraceId = traceId;
        SpanId = spanId;
        Baggage = baggage;
    }

    public string TraceId { get; }
    public string SpanId { get; }
    public string Baggage { get; }
    public string Traceparent => Format(TraceId, SpanId);

    public static TraceContext Create(string launch, string test, int attempt) =>
        new(NonZero(RandomHex(16)), RandomSpanId(), FormatBaggage(launch, test, attempt));

    public static string RandomHex(int bytes) => Convert.ToHexString(RandomNumberGenerator.GetBytes(bytes)).ToLowerInvariant();

    public static string RandomSpanId() => NonZero(RandomHex(8));

    public static string Format(string traceId, string spanId) => $"00-{traceId}-{spanId}-01";

    public static (string TraceId, string SpanId)? Parse(string? value)
    {
        var m = TraceparentPattern.Match((value ?? string.Empty).Trim());
        if (!m.Success || IsZero(m.Groups[1].Value) || IsZero(m.Groups[2].Value))
        {
            return null;
        }
        return (m.Groups[1].Value, m.Groups[2].Value);
    }

    public static string FormatBaggage(string launch, string test, int attempt)
    {
        static string Enc(string v)
        {
            var e = Uri.EscapeDataString(v);
            return e.Length > 256 ? e.Substring(0, 256) : e;
        }
        return $"fastproof.launch={Enc(launch)},fastproof.test={Enc(test)},fastproof.attempt={attempt}";
    }

    /// <summary>Headers for HTTP clients; <paramref name="newSpan"/> gives a new span id per request.</summary>
    public IReadOnlyDictionary<string, string> Headers(bool newSpan = false) => new Dictionary<string, string>
    {
        ["traceparent"] = Format(TraceId, newSpan ? RandomSpanId() : SpanId),
        ["baggage"] = Baggage,
    };

    /// <summary>A copy of the process environment with <c>TRACEPARENT</c> and <c>BAGGAGE</c>, for child processes.</summary>
    public IDictionary<string, string> Env()
    {
        var o = new Dictionary<string, string>();
        foreach (System.Collections.DictionaryEntry e in Environment.GetEnvironmentVariables())
        {
            o[(string)e.Key] = e.Value?.ToString() ?? string.Empty;
        }
        o["TRACEPARENT"] = Traceparent;
        o["BAGGAGE"] = Baggage;
        return o;
    }

    private static bool IsZero(string h) => h.Trim('0').Length == 0;

    private static string NonZero(string h) => IsZero(h) ? h.Substring(0, h.Length - 1) + "1" : h;
}
