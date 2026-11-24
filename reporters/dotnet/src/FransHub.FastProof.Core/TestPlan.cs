using System;
using System.Collections.Generic;
using System.IO;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace FransHub.FastProof;

/// <summary>An Allure-format test plan: <c>{"version": "1.0", "tests": [{"id": "...", "selector": "..."}]}</c>.</summary>
public sealed class TestPlan
{
    private readonly HashSet<string> _ids = new(StringComparer.Ordinal);
    private readonly HashSet<string> _selectors = new(StringComparer.Ordinal);

    private TestPlan()
    {
    }

    public int Count => _ids.Count + _selectors.Count;

    /// <summary>Reads a plan; <c>null</c> when no path is configured. Throws when the file is unreadable (callers fail open).</summary>
    public static TestPlan? Load(string? path)
    {
        if (string.IsNullOrWhiteSpace(path))
        {
            return null;
        }
        var root = JsonNode.Parse(File.ReadAllText(path)) as JsonObject;
        if (root?["tests"] is not JsonArray tests)
        {
            throw new JsonException($"{path}: not a test plan (expected an object with a tests array)");
        }
        var plan = new TestPlan();
        foreach (var t in tests)
        {
            if (t is not JsonObject o)
            {
                continue;
            }
            if (o["id"] is JsonValue idv)
            {
                plan._ids.Add(idv.ToString());
            }
            if (o["selector"] is JsonValue sv && sv.TryGetValue<string>(out var sel) && sel.Trim().Length > 0)
            {
                plan._selectors.Add(sel.Trim());
            }
        }
        return plan;
    }

    public bool Matches(TestDescriptor d)
    {
        foreach (var id in d.PlanIds)
        {
            if (_ids.Contains(id))
            {
                return true;
            }
        }
        foreach (var s in d.PlanSelectors)
        {
            if (_selectors.Contains(s))
            {
                return true;
            }
        }
        return false;
    }
}
