using System;
using System.Collections;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;

namespace FransHub.FastProof;

/// <summary>Settings that override the environment (except <c>FASTPROOF_ENABLED=false</c>, which always wins).</summary>
public sealed class FastProofOptions
{
    public string? Url { get; set; }
    public string? Project { get; set; }
    public string? Launch { get; set; }
    public string? LaunchName { get; set; }
    public string? Environment { get; set; }
    public Dictionary<string, string> Attributes { get; } = new();
    public bool Debug { get; set; }
    public bool? Enabled { get; set; }
    public bool? Offline { get; set; }
    public string? SpoolDir { get; set; }
    public string? FinishLaunch { get; set; }
    public List<string> SecretEnv { get; } = new();
    public List<string> SecretValues { get; } = new();
    public List<string> SecretPatterns { get; } = new();
    public string? TestPlan { get; set; }
}

public sealed class Config
{
    private static readonly Regex ProjectKey = new("^[a-z0-9][a-z0-9-]{1,62}$", RegexOptions.CultureInvariant);

    public bool Enabled { get; set; }
    public string Reason { get; set; } = string.Empty;
    public string Url { get; set; } = string.Empty;
    public string Token { get; set; } = string.Empty;
    public string Project { get; set; } = string.Empty;
    public bool Offline { get; set; }
    public JsonObject Launch { get; set; } = new();
    public bool OwnLaunch { get; set; }
    public string SpoolDir { get; set; } = ".fastproof/spool";
    public long SpoolMaxBytes { get; set; } = 200L * 1024 * 1024;
    public TimeSpan FlushTimeout { get; set; } = TimeSpan.FromSeconds(30);
    public TimeSpan RequestTimeout { get; set; } = TimeSpan.FromSeconds(15);
    public TimeSpan BatchInterval { get; set; } = TimeSpan.FromMilliseconds(200);
    public bool RunningResults { get; set; } = true;
    public bool LiveLogs { get; set; } = true;
    public string FinishLaunch { get; set; } = "auto";
    public List<string> SecretEnv { get; set; } = new();
    public List<string> SecretValues { get; set; } = new();
    public List<string> SecretPatterns { get; set; } = new();
    public IReadOnlyDictionary<string, string> Env { get; set; } = new Dictionary<string, string>();
    public long MaxAttachmentBytes { get; set; } = 512L * 1024 * 1024;
    public bool DirectUploads { get; set; } = true;
    public long DirectUploadMinBytes { get; set; } = 1024 * 1024;
    public bool Quiet { get; set; }
    public string? TestPlanPath { get; set; }
    public string ExternalLaunchId => Launch["externalId"]?.GetValue<string>() ?? string.Empty;

    public static IReadOnlyDictionary<string, string> ProcessEnvironment()
    {
        var d = new Dictionary<string, string>();
        foreach (DictionaryEntry e in System.Environment.GetEnvironmentVariables())
        {
            d[(string)e.Key] = e.Value?.ToString() ?? string.Empty;
        }
        return d;
    }

    public static Config Resolve(FastProofOptions? options = null, IReadOnlyDictionary<string, string>? env = null, string? cwd = null)
    {
        var o = options ?? new FastProofOptions();
        env ??= ProcessEnvironment();
        cwd ??= Directory.GetCurrentDirectory();

        string? Env(string k) => env.TryGetValue(k, out var v) && !string.IsNullOrWhiteSpace(v) ? v : null;
        string? Pick(string? opt, string var) => !string.IsNullOrWhiteSpace(opt) ? opt : Env(var);
        bool? Flag(string? v) => string.IsNullOrWhiteSpace(v) ? null : !new[] { "0", "false", "no", "off" }.Contains(v.Trim().ToLowerInvariant());
        static List<string> Split(string? v) => (v ?? string.Empty).Split(',').Select(s => s.Trim()).Where(s => s.Length > 0).ToList();
        static double Num(string? v, double d) => double.TryParse(v, System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var n) && n >= 0 ? n : d;

        var token = Env("FASTPROOF_TOKEN") ?? string.Empty;
        if (token.Length == 0 && Env("FASTPROOF_TOKEN_FILE") is { } tf)
        {
            try
            {
                token = File.ReadAllText(tf).Trim();
            }
            catch (IOException)
            {
            }
            catch (UnauthorizedAccessException)
            {
            }
        }
        var url = (Pick(o.Url, "FASTPROOF_URL") ?? string.Empty).Trim();
        var project = (Pick(o.Project, "FASTPROOF_PROJECT") ?? string.Empty).Trim();
        var ci = CiDetector.Detect(env);
        var explicitLaunch = (Pick(o.Launch, "FASTPROOF_LAUNCH") ?? string.Empty).Trim();
        var external = explicitLaunch.Length > 0 ? explicitLaunch : ci?.ExternalId ?? $"local-{Guid.NewGuid()}";
        var name = Pick(o.LaunchName, "FASTPROOF_LAUNCH_NAME") ?? ci?.Name
            ?? (explicitLaunch.Length > 0 ? explicitLaunch : $"{(project.Length > 0 ? project : "tests")} {DateTime.UtcNow:yyyy-MM-dd HH:mm}");
        var launch = new JsonObject { ["externalId"] = external, ["name"] = name };
        if (o.Attributes.Count > 0)
        {
            var arr = new JsonArray();
            foreach (var kv in o.Attributes)
            {
                arr.Add(new JsonObject { ["key"] = kv.Key, ["value"] = kv.Value });
            }
            launch["attributes"] = arr;
        }
        if (ci?.Ci is { Count: > 0 })
        {
            launch["ci"] = ci.Ci.DeepClone();
        }
        if (ci?.Vcs is { Count: > 0 })
        {
            launch["vcs"] = ci.Vcs.DeepClone();
        }
        if (Pick(o.Environment, "FASTPROOF_ENVIRONMENT") is { } environment)
        {
            launch["environment"] = new JsonObject { ["key"] = environment.Trim(), ["values"] = new JsonObject() };
        }
        if (o.Debug)
        {
            launch["debug"] = true;
        }

        var enabled = Flag(env.GetValueOrDefault("FASTPROOF_ENABLED")) != false && o.Enabled != false;
        var offline = o.Offline == true || Flag(env.GetValueOrDefault("FASTPROOF_OFFLINE")) == true;
        var reason = enabled ? string.Empty : "disabled";
        if (enabled && url.Length == 0 && !offline)
        {
            enabled = false;
            reason = "FASTPROOF_URL is not set";
        }
        else if (enabled && !ProjectKey.IsMatch(project))
        {
            enabled = false;
            reason = "FASTPROOF_PROJECT is missing or not a valid project key";
        }
        var spool = Pick(o.SpoolDir, "FASTPROOF_SPOOL_DIR") ?? ".fastproof/spool";
        var secretValues = new List<string>(o.SecretValues);
        if (token.Length > 0)
        {
            secretValues.Add(token);
        }
        return new Config
        {
            Enabled = enabled,
            Reason = reason,
            Url = url,
            Token = token,
            Project = project,
            Offline = offline,
            Launch = launch,
            OwnLaunch = explicitLaunch.Length == 0 && ci?.ExternalId == null,
            SpoolDir = Path.GetFullPath(Path.Combine(cwd, spool)),
            SpoolMaxBytes = (long)Num(Env("FASTPROOF_SPOOL_MAX_BYTES"), 200d * 1024 * 1024),
            FlushTimeout = TimeSpan.FromMilliseconds(Num(Env("FASTPROOF_FLUSH_TIMEOUT_MS"), 30000)),
            LiveLogs = Flag(env.GetValueOrDefault("FASTPROOF_LIVE_LOGS")) != false,
            FinishLaunch = string.IsNullOrWhiteSpace(o.FinishLaunch) ? "auto" : o.FinishLaunch!,
            SecretEnv = DefaultsPlus(o.SecretEnv.Concat(Split(Env("FASTPROOF_SECRET_ENV")))),
            SecretValues = secretValues,
            SecretPatterns = o.SecretPatterns.Concat(Split(Env("FASTPROOF_SECRET_PATTERNS"))).ToList(),
            Env = env,
            DirectUploads = !string.Equals(Env("FASTPROOF_DIRECT_UPLOAD_MODE")?.Trim(), "off", StringComparison.OrdinalIgnoreCase),
            DirectUploadMinBytes = Math.Max(1, (long)Num(Env("FASTPROOF_DIRECT_UPLOAD_MIN_BYTES"), 1024 * 1024)),
            Quiet = Flag(env.GetValueOrDefault("FASTPROOF_QUIET")) == true,
            TestPlanPath = o.TestPlan ?? Env("FASTPROOF_TESTPLAN") ?? Env("ALLURE_TESTPLAN_PATH"),
        };
    }

    private static List<string> DefaultsPlus(IEnumerable<string> extra) => Masker.DefaultSecretEnv.Concat(extra).ToList();
}
