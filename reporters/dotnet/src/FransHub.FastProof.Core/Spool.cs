using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;

namespace FransHub.FastProof;

internal sealed class BlobRef
{
    public string Sha256 = string.Empty;
    public string Name = string.Empty;
    public string ContentType = string.Empty;
    public long Size;
}

/// <summary>One reporter process's spool session (format v1 of reporters/PROTOCOL.md, shared with the other reporters).</summary>
internal sealed class SpoolSession
{
    public const int SpoolVersion = 1;
    public const string BlobPrefix = "blob:";

    private readonly string _root;
    private readonly long _maxBytes;
    private readonly JsonObject _meta;
    private long _used;
    private readonly object _lock = new();

    public SpoolSession(string root, string sessionId, JsonObject meta, long maxBytes)
    {
        _root = root;
        Id = sessionId;
        Dir = Path.Combine(root, sessionId);
        _meta = meta;
        _maxBytes = maxBytes;
        _used = Directory.Exists(root) ? DirSize(root) : 0;
    }

    public string Id { get; }
    public string Dir { get; }
    public bool Opened { get; private set; }
    public int DroppedResults { get; private set; }
    public int DroppedBlobs { get; private set; }
    public int WrittenResults { get; private set; }

    public static long DirSize(string path)
    {
        long total = 0;
        try
        {
            foreach (var f in new DirectoryInfo(path).EnumerateFiles("*", SearchOption.AllDirectories))
            {
                total += f.Length;
            }
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
        }
        return total;
    }

    public static void WriteAtomic(string path, string data)
    {
        var tmp = $"{path}.{Environment.ProcessId}.tmp";
        File.WriteAllText(tmp, data, new UTF8Encoding(false));
        File.Move(tmp, path, true);
    }

    private bool Open()
    {
        if (Opened)
        {
            return true;
        }
        try
        {
            Directory.CreateDirectory(Path.Combine(Dir, "blobs"));
            WriteAtomic(Path.Combine(Dir, "meta.json"), Api.Serialize(_meta));
            Opened = true;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return false;
        }
        return true;
    }

    public bool AddBlob(string sha256, long size, bool low, string? path, byte[]? body)
    {
        lock (_lock)
        {
            if (!Open())
            {
                return false;
            }
            var target = Path.Combine(Dir, "blobs", sha256);
            if (File.Exists(target))
            {
                return true;
            }
            var limit = low ? _maxBytes / 2.0 : _maxBytes;
            if (_used + size > limit)
            {
                DroppedBlobs++;
                return false;
            }
            var tmp = $"{target}.{Environment.ProcessId}.tmp";
            try
            {
                if (path != null)
                {
                    File.Copy(path, tmp, true);
                }
                else
                {
                    File.WriteAllBytes(tmp, body ?? Array.Empty<byte>());
                }
                File.Move(tmp, target, true);
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
                DroppedBlobs++;
                return false;
            }
            _used += size;
            return true;
        }
    }

    public bool Append(JsonObject entry)
    {
        lock (_lock)
        {
            if (!Open())
            {
                DroppedResults++;
                return false;
            }
            var line = Encoding.UTF8.GetBytes(Api.Serialize(entry) + "\n");
            if (_used + line.Length > _maxBytes)
            {
                DroppedResults++;
                return false;
            }
            try
            {
                using var fs = new FileStream(Path.Combine(Dir, "results.ndjson"), FileMode.Append, FileAccess.Write, FileShare.Read);
                fs.Write(line, 0, line.Length);
                fs.Flush(true);
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
                DroppedResults++;
                return false;
            }
            _used += line.Length;
            WrittenResults++;
            return true;
        }
    }

    public void End(string? finish)
    {
        lock (_lock)
        {
            if (!Opened)
            {
                return;
            }
            _meta["ended"] = true;
            if (finish != null)
            {
                _meta["finish"] = finish;
            }
            try
            {
                WriteAtomic(Path.Combine(Dir, "meta.json"), Api.Serialize(_meta));
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
            }
        }
    }
}

internal sealed class FlushResult
{
    public int Sessions;
    public int Results;
    public int Failed;
    public int Remaining;
    public List<string> Errors = new();
}

internal static class SpoolFlusher
{
    private static readonly Regex BlobName = new("^[0-9a-f]{64}$", RegexOptions.CultureInvariant);

    internal static bool IsBlobName(string sha) => BlobName.IsMatch(sha);

    /// <summary>Opens a spooled blob for one upload attempt and checks that its content hashes to its name, so a
    /// planted spool line or a link swapped into <c>blobs/</c> cannot send another file with this run's token.</summary>
    internal static Stream OpenVerified(string path, string sha)
    {
        var fs = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read);
        try
        {
            var actual = Convert.ToHexString(SHA256.HashData(fs)).ToLowerInvariant();
            if (actual != sha)
            {
                throw new IOException($"the spooled attachment {sha} does not match its hash; skipped");
            }
            fs.Position = 0;
            return fs;
        }
        catch
        {
            fs.Dispose();
            throw;
        }
    }

    private static bool PidAlive(int pid)
    {
        try
        {
            using var p = Process.GetProcessById(pid);
            return !p.HasExited;
        }
        catch (ArgumentException)
        {
            return false;
        }
        catch (InvalidOperationException)
        {
            return false;
        }
    }

    public static List<(string Id, string Dir, JsonObject Meta)> ListSessions(string root, string? exclude)
    {
        var o = new List<(string, string, JsonObject)>();
        string[] names;
        try
        {
            names = Directory.GetDirectories(root).Select(Path.GetFileName).OfType<string>().OrderBy(s => s, StringComparer.Ordinal).ToArray();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return o;
        }
        foreach (var name in names)
        {
            if (name == exclude || name.StartsWith('.'))
            {
                continue;
            }
            var d = Path.Combine(root, name);
            JsonObject? meta;
            try
            {
                meta = JsonNode.Parse(File.ReadAllText(Path.Combine(d, "meta.json"))) as JsonObject;
            }
            catch (Exception e) when (e is IOException or JsonException or UnauthorizedAccessException)
            {
                continue;
            }
            if (meta == null || meta["v"]?.GetValue<int>() != SpoolSession.SpoolVersion)
            {
                continue;
            }
            var pid = meta["pid"]?.GetValue<int>() ?? 0;
            var running = meta["ended"]?.GetValue<bool>() != true
                && meta["host"]?.GetValue<string>() == Environment.MachineName
                && pid != Environment.ProcessId
                && PidAlive(pid);
            if (!running)
            {
                o.Add((name, d, meta));
            }
        }
        return o;
    }

    private static void ReplaceBlobIds(JsonNode? node, Dictionary<string, string> ids)
    {
        if (node is not JsonObject obj)
        {
            return;
        }
        if (obj["attachments"] is JsonArray atts)
        {
            var kept = new List<JsonNode>();
            foreach (var a in atts.ToList())
            {
                var id = a?["id"]?.GetValue<string>() ?? string.Empty;
                if (id.StartsWith(SpoolSession.BlobPrefix, StringComparison.Ordinal))
                {
                    if (!ids.TryGetValue(id.Substring(SpoolSession.BlobPrefix.Length), out var real))
                    {
                        continue;
                    }
                    a![ "id"] = real;
                }
                kept.Add(a!);
            }
            var fresh = new JsonArray();
            foreach (var k in kept)
            {
                fresh.Add(k.DeepClone());
            }
            obj["attachments"] = fresh;
        }
        if (obj["steps"] is JsonArray steps)
        {
            foreach (var s in steps)
            {
                ReplaceBlobIds(s, ids);
            }
        }
    }

    public static FlushResult Flush(Api api, string root, string? exclude, DateTime? deadline, string? project, int batch = 200)
    {
        var outp = new FlushResult();
        bool Late() => deadline.HasValue && DateTime.UtcNow > deadline.Value;
        foreach (var (_, dir, meta) in ListSessions(root, exclude))
        {
            if (project != null && meta["project"]?.GetValue<string>() != project)
            {
                continue;
            }
            List<string> lines;
            try
            {
                lines = File.ReadAllText(Path.Combine(dir, "results.ndjson")).Split('\n').Where(l => l.Trim().Length > 0).ToList();
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
                lines = new List<string>();
            }
            var done = 0;
            try
            {
                done = JsonNode.Parse(File.ReadAllText(Path.Combine(dir, "progress.json")))?["lines"]?.GetValue<int>() ?? 0;
            }
            catch (Exception e) when (e is IOException or JsonException or UnauthorizedAccessException)
            {
            }
            if (Late())
            {
                outp.Remaining += lines.Count - done;
                continue;
            }
            try
            {
                var launch = api.StartLaunch((JsonObject)meta["launch"]!.DeepClone(), 2);
                var launchId = launch["id"]!.GetValue<string>();
                var blobIds = new Dictionary<string, string>();
                while (done < lines.Count && !Late())
                {
                    var chunk = lines.Skip(done).Take(batch).ToList();
                    var results = new List<JsonNode>();
                    foreach (var ln in chunk)
                    {
                        JsonObject? e;
                        try
                        {
                            e = JsonNode.Parse(ln) as JsonObject;
                        }
                        catch (JsonException)
                        {
                            e = null;
                        }
                        if (e?["result"] is not JsonObject result)
                        {
                            outp.Failed++;
                            continue;
                        }
                        foreach (var b in (e["blobs"] as JsonArray) ?? new JsonArray())
                        {
                            var sha = b!["sha256"]!.GetValue<string>();
                            if (blobIds.ContainsKey(sha) || !IsBlobName(sha))
                            {
                                continue;
                            }
                            var p = Path.Combine(dir, "blobs", sha);
                            if (!File.Exists(p) || new FileInfo(p).LinkTarget != null)
                            {
                                continue;
                            }
                            JsonObject r;
                            try
                            {
                                r = api.UploadAttachment(launchId, b["name"]!.GetValue<string>(), b["contentType"]!.GetValue<string>(), null, null, 2,
                                    () => OpenVerified(p, sha));
                            }
                            catch (ApiException x) when (x.Status < 0)
                            {
                                outp.Errors.Add(x.Message);
                                continue;
                            }
                            blobIds[sha] = r["id"]!.GetValue<string>();
                        }
                        ReplaceBlobIds(result, blobIds);
                        results.Add(result.DeepClone());
                    }
                    if (results.Count > 0)
                    {
                        var summary = api.PostResults(launchId, results, 2);
                        outp.Results += summary.Accepted + summary.Duplicates;
                        outp.Failed += summary.Rejected.Count;
                        outp.Errors.AddRange(summary.Rejected.Take(3).Select(r => r.Message));
                    }
                    done += chunk.Count;
                    SpoolSession.WriteAtomic(Path.Combine(dir, "progress.json"), $"{{\"lines\":{done}}}");
                }
                if (done >= lines.Count)
                {
                    if (meta["ended"]?.GetValue<bool>() == true && meta["finish"]?.GetValue<string>() is { } fin)
                    {
                        api.FinishLaunch(launchId, fin, 2);
                    }
                    if (meta["ended"]?.GetValue<bool>() == true || meta["pid"]?.GetValue<int>() != Environment.ProcessId)
                    {
                        try
                        {
                            Directory.Delete(dir, true);
                        }
                        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
                        {
                        }
                    }
                    outp.Sessions++;
                }
                else
                {
                    outp.Remaining += lines.Count - done;
                }
            }
            catch (ApiException e)
            {
                outp.Remaining += lines.Count - done;
                outp.Errors.Add(e.Message);
                if (!e.Fatal)
                {
                    break;
                }
            }
            catch (Exception e) when (e is IOException or KeyNotFoundException or JsonException or InvalidOperationException or UnauthorizedAccessException)
            {
                outp.Remaining += lines.Count - done;
                outp.Errors.Add(e.Message);
            }
        }
        return outp;
    }
}
