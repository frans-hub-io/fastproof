using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.RegularExpressions;

namespace FransHub.FastProof;

/// <summary>Replaces secrets with <c>****</c>: values of matching environment variables, literal values and patterns.</summary>
public sealed class Masker
{
    public const string Mask = "****";
    public const int MinSecretLength = 4;

    public static readonly string[] DefaultSecretEnv =
    {
        "FASTPROOF_TOKEN", "*TOKEN*", "*SECRET*", "*PASSWORD*", "*PASSWD*", "*_KEY", "*APIKEY*", "*API_KEY*", "*CREDENTIAL*",
    };

    private static readonly HashSet<string> NotSecret = new(StringComparer.OrdinalIgnoreCase)
    {
        "true", "false", "yes", "no", "on", "off", "null", "none", "undefined",
    };

    private readonly Regex? _literal;
    private readonly List<Regex> _patterns = new();

    public Masker(IEnumerable<string>? envNames, IEnumerable<string>? values, IEnumerable<string>? patterns,
        IReadOnlyDictionary<string, string>? env)
    {
        var globs = (envNames ?? DefaultSecretEnv).Select(g => GlobToRegex(g)).ToList();
        var found = new HashSet<string>(StringComparer.Ordinal);

        void Add(string? v)
        {
            if (string.IsNullOrEmpty(v))
            {
                return;
            }
            var t = v.Trim();
            if (t.Length < MinSecretLength || NotSecret.Contains(t) || (t.Length <= 6 && t.All(char.IsDigit)))
            {
                return;
            }
            found.Add(t);
            var enc = Uri.EscapeDataString(t);
            if (enc != t)
            {
                found.Add(enc);
            }
        }

        foreach (var kv in env ?? new Dictionary<string, string>())
        {
            if (globs.Any(g => g.IsMatch(kv.Key)))
            {
                Add(kv.Value);
            }
        }
        foreach (var v in values ?? Array.Empty<string>())
        {
            Add(v);
        }
        var ordered = found.OrderByDescending(s => s.Length).ToList();
        if (ordered.Count > 0)
        {
            _literal = new Regex(string.Join("|", ordered.Select(Regex.Escape)), RegexOptions.CultureInvariant);
        }
        foreach (var p in patterns ?? Array.Empty<string>())
        {
            try
            {
                _patterns.Add(new Regex(p, RegexOptions.CultureInvariant, TimeSpan.FromSeconds(1)));
            }
            catch (ArgumentException)
            {
            }
        }
    }

    public static Masker None { get; } = new(Array.Empty<string>(), null, null, null);

    public bool Active => _literal != null || _patterns.Count > 0;

    public string? Apply(string? s)
    {
        if (string.IsNullOrEmpty(s))
        {
            return s;
        }
        var o = s;
        if (_literal != null)
        {
            o = _literal.Replace(o, Mask);
        }
        foreach (var p in _patterns)
        {
            try
            {
                o = p.Replace(o, Mask);
            }
            catch (RegexMatchTimeoutException)
            {
            }
        }
        return o;
    }

    public string Text(string s) => Apply(s) ?? string.Empty;

    private static Regex GlobToRegex(string glob)
    {
        var sb = new StringBuilder("^");
        foreach (var c in glob.ToUpperInvariant())
        {
            sb.Append(c switch { '*' => ".*", '?' => ".", _ => Regex.Escape(c.ToString()) });
        }
        sb.Append('$');
        return new Regex(sb.ToString(), RegexOptions.CultureInvariant | RegexOptions.IgnoreCase);
    }
}
