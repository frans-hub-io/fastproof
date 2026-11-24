using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;

namespace FransHub.FastProof;

/// <summary>Helpers adapters share: FastProof attributes, parameter values, selectors for test plans.</summary>
public static class Metadata
{
    public const int MaxParameterValue = 1000;

    /// <summary>Reads <see cref="FastProofIdAttribute"/>, <see cref="FastProofLabelAttribute"/> and <see cref="FastProofLinkAttribute"/>
    /// from the assembly, type (and its declaring types) and method.</summary>
    public static void Apply(TestDescriptor d, MethodInfo? method)
    {
        if (method == null)
        {
            return;
        }
        var types = new List<Type>();
        for (var t = method.DeclaringType; t != null; t = t.DeclaringType)
        {
            types.Insert(0, t);
        }
        var members = new List<ICustomAttributeProvider>();
        if (method.DeclaringType != null)
        {
            members.Add(method.DeclaringType.Assembly);
        }
        members.AddRange(types);
        members.Add(method);
        foreach (var m in members)
        {
            foreach (var a in m.GetCustomAttributes(typeof(FastProofIdAttribute), false).Cast<FastProofIdAttribute>())
            {
                d.TestCaseId = a.Id;
            }
            foreach (var a in m.GetCustomAttributes(typeof(FastProofLabelAttribute), false).Cast<FastProofLabelAttribute>())
            {
                d.Labels.Add(new LabelItem(a.Name, a.Value));
            }
            foreach (var a in m.GetCustomAttributes(typeof(FastProofLinkAttribute), false).Cast<FastProofLinkAttribute>())
            {
                d.Links.Add(new LinkItem(a.Url, a.Name, a.Type));
            }
        }
        if (d.TestCaseId != null)
        {
            d.PlanIds.Add(d.TestCaseId);
        }
    }

    /// <summary>A short printable form of a parameter value.</summary>
    public static string Format(object? value)
    {
        string s;
        try
        {
            s = value switch
            {
                null => "null",
                string str => str,
                char c => c.ToString(),
                bool b => b ? "true" : "false",
                IFormattable f => f.ToString(null, System.Globalization.CultureInfo.InvariantCulture) ?? string.Empty,
                System.Collections.IEnumerable e => "[" + string.Join(", ", e.Cast<object?>().Take(20).Select(Format)) + "]",
                _ => value.ToString() ?? string.Empty,
            };
        }
        catch (Exception)
        {
            s = value?.GetType().Name ?? "null";
        }
        return s.Length > MaxParameterValue ? s.Substring(0, MaxParameterValue) + "…" : s;
    }

    /// <summary>Adds the parameters of a test method: names from the method signature, values from the arguments.</summary>
    public static void AddParameters(TestDescriptor d, MethodInfo? method, IReadOnlyList<object?>? args)
    {
        if (args == null || args.Count == 0)
        {
            return;
        }
        var names = method?.GetParameters().Select(p => p.Name ?? string.Empty).ToArray() ?? Array.Empty<string>();
        for (var i = 0; i < args.Count; i++)
        {
            var name = i < names.Length && names[i].Length > 0 ? names[i] : $"arg{i}";
            d.Parameters.Add(new ParameterItem(name, Format(args[i])));
        }
    }

    /// <summary>Selectors for test plans: <c>Namespace.Type.Method</c>, <c>Namespace.Type#Method</c> and the same with the display name.</summary>
    public static void AddSelectors(TestDescriptor d, string? typeFullName, string? methodName, string? displayFullName)
    {
        if (typeFullName != null && methodName != null)
        {
            d.PlanSelectors.Add($"{typeFullName}.{methodName}");
            d.PlanSelectors.Add($"{typeFullName}#{methodName}");
        }
        if (!string.IsNullOrEmpty(displayFullName))
        {
            d.PlanSelectors.Add(displayFullName);
        }
    }
}
