using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net.Http;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;

namespace FransHub.FastProof;

/// <summary>An attachment of an attempt: a file on disk or bytes.</summary>
internal sealed class FileRef
{
    public string Name = string.Empty;
    public string ContentType = "application/octet-stream";
    public string? Path;
    public byte[]? Body;
}

internal sealed class PreparedFile
{
    public string Name = string.Empty;
    public string ContentType = string.Empty;
    public string? Path;
    public byte[]? Body;
    public string Sha256 = string.Empty;
    public long Size;
}

public sealed class RunSummary
{
    public string? LaunchId { get; internal set; }
    public int? LaunchNumber { get; internal set; }
    public int Sent { get; internal set; }
    public int Spooled { get; internal set; }
    public int Lost { get; internal set; }
    public int Attachments { get; internal set; }
    public bool Offline { get; internal set; }
    public string OfflineReason { get; internal set; } = string.Empty;
    public string? SpoolDir { get; internal set; }
    public string Logs { get; internal set; } = "unknown";
}

/// <summary>The transport: a launch, a background sender, live logs with feature detection, attachment uploads
/// with sha256 dedup, a circuit breaker and the offline spool. Nothing here throws into the test run.</summary>
internal sealed class Client
{
    public const int MaxLogMessage = 64 * 1024;
    public const int MaxQueuedLogs = 20000;
    public const int ResultBatch = 500;
    public const int ResultBatchBytes = 4 * 1024 * 1024;
    public const int BreakerFailures = 5;
    public const int MaskTextAttachmentBytes = 4 * 1024 * 1024;
    private static readonly TimeSpan LogsDetectWait = TimeSpan.FromSeconds(2);

    private sealed class QueueItem
    {
        public bool Running;
        public JsonObject Result = new();
        public int Bytes;
    }

    private sealed class Inflight
    {
        public JsonObject Result = new();
        public List<FileRef> Files = new();
        public bool Passed;
        public string? Fallback;
    }

    private readonly Config _cfg;
    private readonly Action<string> _out;
    private readonly Api _api;
    private readonly object _lock = new();
    private readonly List<QueueItem> _queue = new();
    private List<JsonObject> _logs = new();
    private readonly Dictionary<string, int> _seq = new();
    private readonly Dictionary<string, Inflight> _inflight = new();
    private readonly Dictionary<string, string> _uploads = new();
    private readonly Dictionary<string, object> _uploadLocks = new();
    private readonly HashSet<string> _warned = new();
    private readonly System.Collections.Concurrent.BlockingCollection<Inflight> _jobs = new();
    private readonly List<Thread> _workers = new();
    private readonly object _sendLock = new();
    private readonly object _launchLock = new();
    private readonly SpoolSession _spool;
    private string _directOff = string.Empty;
    private bool _stop;
    private Thread? _thread;
    private Thread? _oldSpool;
    private int _failures;
    private int _sent;
    private int _lost;
    private int _attachments;
    private int _droppedLogs;

    public Client(Config cfg, Action<string>? output = null)
    {
        _cfg = cfg;
        _out = output ?? (m => Console.Error.WriteLine(m));
        _api = new Api(cfg.Url, cfg.Token, cfg.Project, cfg.RequestTimeout);
        Masker = new Masker(cfg.SecretEnv, cfg.SecretValues, cfg.SecretPatterns, cfg.Env);
        Offline = cfg.Offline;
        OfflineReason = cfg.Offline ? "offline mode" : string.Empty;
        Running = cfg.RunningResults;
        LogsState = cfg.LiveLogs ? "unknown" : "off";
        var stamp = DateTime.UtcNow.ToString("yyyyMMdd'T'HHmmss");
        SessionId = $"{stamp}-{Environment.ProcessId}-{TraceContext.RandomHex(4)}";
        _spool = new SpoolSession(cfg.SpoolDir, SessionId, new JsonObject
        {
            ["v"] = SpoolSession.SpoolVersion,
            ["reporter"] = $"dotnet/{Api.Version}",
            ["project"] = cfg.Project,
            ["launch"] = cfg.Launch.DeepClone(),
            ["createdAt"] = DateTime.UtcNow.ToString("o"),
            ["pid"] = Environment.ProcessId,
            ["host"] = Environment.MachineName,
        }, cfg.SpoolMaxBytes);
    }

    public Masker Masker { get; }
    public string SessionId { get; }
    public string? LaunchId { get; private set; }
    public int? LaunchNumber { get; private set; }
    public bool Offline { get; private set; }
    public string OfflineReason { get; private set; }
    public bool Running { get; private set; }
    public string LogsState { get; private set; }

    public void Info(string msg)
    {
        if (!_cfg.Quiet)
        {
            _out($"[fastproof] {msg}");
        }
    }

    public void WarnOnce(string key, string msg)
    {
        lock (_lock)
        {
            if (!_warned.Add(key))
            {
                return;
            }
        }
        _out($"[fastproof] {Masker.Text(msg)}");
    }

    public void Start(bool flushOld = true)
    {
        if (Offline)
        {
            return;
        }
        _thread = new Thread(Loop) { Name = "fastproof-sender", IsBackground = true };
        _thread.Start();
        if (flushOld)
        {
            _oldSpool = new Thread(FlushOld) { Name = "fastproof-spool", IsBackground = true };
            _oldSpool.Start();
        }
    }

    private void FlushOld()
    {
        try
        {
            EnsureLaunch(2);
            if (Offline)
            {
                return;
            }
            var r = SpoolFlusher.Flush(_api, _cfg.SpoolDir, SessionId, null, _cfg.Project);
            if (r.Sessions > 0)
            {
                Info($"sent {r.Results} results left by earlier runs from {_cfg.SpoolDir}");
            }
        }
        catch (Exception)
        {
        }
    }

    private void EnsureLaunch(int retries = 0)
    {
        lock (_launchLock)
        {
            EnsureLaunchLocked(retries);
        }
    }

    private void EnsureLaunchLocked(int retries)
    {
        if (LaunchId != null || Offline)
        {
            return;
        }
        JsonObject launch;
        try
        {
            launch = _api.StartLaunch(_cfg.Launch, retries);
        }
        catch (ApiException e)
        {
            Failed(e);
            return;
        }
        lock (_lock)
        {
            if (LaunchId == null)
            {
                LaunchId = launch["id"]!.GetValue<string>();
                LaunchNumber = launch["number"] is JsonValue v && v.TryGetValue<int>(out var n) ? n : null;
            }
            _failures = 0;
        }
    }

    private void Failed(Exception e)
    {
        var err = e as ApiException ?? new ApiException(e.Message, 0, false);
        if (err.Fatal)
        {
            if (err.Status is 401 or 403 or 404)
            {
                GoOffline(err.Message);
            }
            else
            {
                WarnOnce($"fatal:{err.Status}", $"the server refused a request: {err.Message}");
            }
            return;
        }
        var n = Interlocked.Increment(ref _failures);
        if (n >= BreakerFailures)
        {
            GoOffline(err.Message);
        }
    }

    private void GoOffline(string reason)
    {
        List<QueueItem> queued;
        lock (_lock)
        {
            if (Offline)
            {
                return;
            }
            Offline = true;
            OfflineReason = reason;
            queued = _queue.ToList();
            _queue.Clear();
            _logs = new List<JsonObject>();
        }
        WarnOnce("offline", $"reporting continues offline ({reason}); results go to {_cfg.SpoolDir}");
        foreach (var q in queued.Where(q => !q.Running))
        {
            SpoolResult(q.Result, new List<PreparedFile>(), true);
        }
    }

    public void OpenRunning(JsonObject result)
    {
        if (!Running || Offline)
        {
            return;
        }
        var r = (JsonObject)result.DeepClone();
        r["status"] = "running";
        Enqueue(r, true);
    }

    public void Log(string resultId, int attempt, string level, string stream, string message, string? step = null)
    {
        if (LogsState == "off" || Offline)
        {
            return;
        }
        var line = new JsonObject
        {
            ["resultId"] = resultId,
            ["attempt"] = attempt,
            ["ts"] = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
            ["level"] = level,
            ["stream"] = stream,
            ["message"] = Cut(Masker.Text(message), MaxLogMessage),
        };
        if (!string.IsNullOrEmpty(step))
        {
            line["step"] = Masker.Text(step);
        }
        lock (_lock)
        {
            var key = $"{resultId}\0{attempt}";
            _seq.TryGetValue(key, out var seq);
            _seq[key] = seq + 1;
            line["seq"] = seq;
            _logs.Add(line);
            if (_logs.Count > MaxQueuedLogs)
            {
                _logs.RemoveRange(0, _logs.Count - MaxQueuedLogs);
                _droppedLogs++;
            }
        }
    }

    public static string Cut(string s, int limit) => s.Length <= limit ? s : s.Substring(0, limit) + "\n…[truncated]";

    public static bool IsText(string contentType)
    {
        var b = contentType.Split(';')[0].Trim().ToLowerInvariant();
        return b.StartsWith("text/", StringComparison.Ordinal) || b is "application/json" or "application/x-ndjson" || b.EndsWith("+json", StringComparison.Ordinal);
    }

    private void Enqueue(JsonObject result, bool running)
    {
        var size = Encoding.UTF8.GetByteCount(Api.Serialize(result));
        lock (_lock)
        {
            _queue.Add(new QueueItem { Running = running, Result = result, Bytes = size });
            if (_queue.Count >= ResultBatch)
            {
                Monitor.PulseAll(_lock);
            }
        }
    }

    public void Finish(JsonObject result, List<FileRef> files, bool passed, string? fallbackLogs)
    {
        var rid = result["id"]!.GetValue<string>();
        var inflight = new Inflight { Result = result, Files = files, Passed = passed, Fallback = fallbackLogs };
        lock (_lock)
        {
            _inflight[rid] = inflight;
        }
        try
        {
            EnsureWorkers();
            _jobs.Add(inflight);
        }
        catch (InvalidOperationException)
        {
            RunPipeline(inflight);
            lock (_lock)
            {
                _inflight.Remove(rid);
            }
        }
    }

    private void EnsureWorkers()
    {
        if (_workers.Count > 0)
        {
            return;
        }
        lock (_lock)
        {
            if (_workers.Count > 0)
            {
                return;
            }
            for (var i = 0; i < 4; i++)
            {
                var t = new Thread(Work) { Name = $"fastproof-upload-{i}", IsBackground = true };
                _workers.Add(t);
                t.Start();
            }
        }
    }

    private void Work()
    {
        foreach (var job in _jobs.GetConsumingEnumerable())
        {
            try
            {
                RunPipeline(job);
            }
            finally
            {
                lock (_lock)
                {
                    _inflight.Remove(job.Result["id"]!.GetValue<string>());
                    Monitor.PulseAll(_lock);
                }
            }
        }
    }

    private PreparedFile? Prepare(FileRef f)
    {
        try
        {
            var body = f.Body;
            var path = f.Path;
            var size = body?.LongLength ?? new FileInfo(path ?? string.Empty).Length;
            if (size > _cfg.MaxAttachmentBytes)
            {
                WarnOnce($"big:{f.Name}", $"attachment {f.Name} is too large; skipped");
                return null;
            }
            if (Masker.Active && IsText(f.ContentType) && size <= MaskTextAttachmentBytes)
            {
                var raw = body ?? File.ReadAllBytes(path ?? string.Empty);
                var text = new UTF8Encoding(false, false).GetString(raw);
                var masked = Masker.Text(text);
                if (masked != text || body != null)
                {
                    body = Encoding.UTF8.GetBytes(masked);
                    path = null;
                    size = body.LongLength;
                }
            }
            string sha;
            if (body != null)
            {
                sha = Convert.ToHexString(SHA256.HashData(body)).ToLowerInvariant();
            }
            else
            {
                using var fs = new FileStream(path!, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                sha = Convert.ToHexString(SHA256.HashData(fs)).ToLowerInvariant();
            }
            return new PreparedFile { Name = f.Name, ContentType = f.ContentType, Path = path, Body = body, Sha256 = sha, Size = size };
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or ArgumentException)
        {
            WarnOnce($"read:{f.Name}", $"cannot read attachment {f.Name}: {e.Message}");
            return null;
        }
    }

    private string Upload(PreparedFile p)
    {
        object l;
        lock (_lock)
        {
            if (!_uploadLocks.TryGetValue(p.Sha256, out var existing))
            {
                existing = new object();
                _uploadLocks[p.Sha256] = existing;
            }
            l = existing;
        }
        lock (l)
        {
            return UploadOnce(p);
        }
    }

    private string UploadOnce(PreparedFile p)
    {
        lock (_lock)
        {
            if (_uploads.TryGetValue(p.Sha256, out var known))
            {
                return known;
            }
        }
        if (Offline || LaunchId == null)
        {
            return "retry";
        }
        string id;
        try
        {
            var direct = Direct(p);
            if (direct != null)
            {
                id = direct;
            }
            else
            {
                id = _api.UploadAttachment(LaunchId, p.Name, p.ContentType, p.Path, p.Body, 2)["id"]!.GetValue<string>();
            }
        }
        catch (ApiException e)
        {
            if (e.Fatal && (e.Status < 0 || e.Status is 400 or 413 or 415))
            {
                WarnOnce($"att:{e.Status}", $"attachment {p.Name} rejected: {e.Message}");
                return "skip";
            }
            Failed(e);
            return "retry";
        }
        Interlocked.Exchange(ref _failures, 0);
        Interlocked.Increment(ref _attachments);
        lock (_lock)
        {
            _uploads[p.Sha256] = id;
        }
        return id;
    }

    private string? Direct(PreparedFile p)
    {
        if (_directOff.Length > 0 || !_cfg.DirectUploads || p.Size < _cfg.DirectUploadMinBytes)
        {
            return null;
        }
        var launch = LaunchId ?? string.Empty;
        JsonObject t;
        try
        {
            t = _api.DirectUpload(launch, p.Sha256, p.Size, p.Name, p.ContentType, 2);
        }
        catch (ApiException e) when (e.Fatal)
        {
            if (e.Status is 404 or 405 or 501)
            {
                _directOff = "the server does not offer direct uploads";
            }
            else
            {
                WarnOnce($"direct:a{e.Status}", $"direct upload of {p.Name} refused ({e.Message}); sent through the server");
            }
            return null;
        }
        var status = t["status"]?.GetValue<string>();
        if (status == "exists" && t["id"]?.GetValue<string>() is { } existing)
        {
            return existing;
        }
        if (status != "upload" || t["url"]?.GetValue<string>() is not { } url || t["uploadId"]?.GetValue<string>() is not { } uploadId)
        {
            return null;
        }
        var headers = new Dictionary<string, string>();
        if (t["headers"] is JsonObject h)
        {
            foreach (var kv in h)
            {
                headers[kv.Key] = kv.Value?.GetValue<string>() ?? string.Empty;
            }
        }
        var code = _api.PutPresigned(url, headers, p.Size, p.Path, p.Body, TimeSpan.FromSeconds(120 + p.Size / (512.0 * 1024)));
        if (code == 0)
        {
            _directOff = "the S3 store is not reachable";
            WarnOnce("direct:unreachable", "direct uploads off: the S3 store is not reachable; attachments go through the server");
            return null;
        }
        if (code is < 200 or >= 300)
        {
            WarnOnce($"direct:{code}", $"the S3 store answered {code} to a direct upload; sent through the server");
            return null;
        }
        try
        {
            return _api.CompleteDirect(launch, uploadId, 2);
        }
        catch (ApiException e) when (e.Fatal)
        {
            WarnOnce($"direct:c{e.Status}", $"direct upload of {p.Name} not accepted ({e.Message}); sent through the server");
            return null;
        }
    }

    private void ProbeLogs()
    {
        if (LogsState != "unknown" || Offline || LaunchId == null)
        {
            return;
        }
        try
        {
            if (_api.PostLogs(LaunchId, Array.Empty<JsonNode>(), 0) == "unsupported")
            {
                LogsState = "off";
                lock (_lock)
                {
                    _logs = new List<JsonObject>();
                }
            }
            else
            {
                LogsState = "on";
            }
        }
        catch (ApiException)
        {
        }
    }

    private void FoldLogs(JsonObject result, string? text)
    {
        if (string.IsNullOrEmpty(text) || LogsState == "on")
        {
            return;
        }
        var o = result["stdout"]?.GetValue<string>() ?? string.Empty;
        var sep = o.Length > 0 && !o.EndsWith('\n') ? "\n" : string.Empty;
        result["stdout"] = $"{o}{sep}--- log ---\n{text}\n";
    }

    private void RunPipeline(Inflight f)
    {
        try
        {
            var prepared = f.Files.Select(Prepare).OfType<PreparedFile>().ToList();
            if (LaunchId == null && !Offline)
            {
                EnsureLaunch(1);
            }
            if (!string.IsNullOrEmpty(f.Fallback))
            {
                ProbeLogs();
                var until = DateTime.UtcNow + LogsDetectWait;
                while (LogsState == "unknown" && !Offline && DateTime.UtcNow < until)
                {
                    Thread.Sleep(50);
                }
                FoldLogs(f.Result, f.Fallback);
            }
            var refs = new List<JsonObject>();
            var pending = new List<PreparedFile>();
            foreach (var p in prepared)
            {
                var id = Upload(p);
                if (id == "retry")
                {
                    pending.Add(p);
                }
                else if (id != "skip")
                {
                    refs.Add(new JsonObject { ["id"] = id, ["name"] = p.Name, ["contentType"] = p.ContentType });
                }
            }
            AppendAttachments(f.Result, refs);
            if (Offline || pending.Count > 0)
            {
                SpoolResult(f.Result, pending, f.Passed);
                return;
            }
            Enqueue(f.Result, false);
        }
        catch (Exception e)
        {
            WarnOnce("pipeline", $"reporting error (ignored): {e.Message}");
        }
    }

    private static void AppendAttachments(JsonObject result, IEnumerable<JsonObject> refs)
    {
        var arr = new JsonArray();
        foreach (var a in (result["attachments"] as JsonArray)?.ToList() ?? new List<JsonNode?>())
        {
            arr.Add(a?.DeepClone());
        }
        foreach (var r in refs)
        {
            arr.Add(r);
        }
        result["attachments"] = arr;
    }

    private void SpoolResult(JsonObject result, List<PreparedFile> blobs, bool passed)
    {
        var kept = new JsonArray();
        var refs = new List<JsonObject>();
        foreach (var b in blobs)
        {
            if (_spool.AddBlob(b.Sha256, b.Size, passed, b.Path, b.Body))
            {
                kept.Add(new JsonObject { ["sha256"] = b.Sha256, ["name"] = b.Name, ["contentType"] = b.ContentType, ["size"] = b.Size });
                refs.Add(new JsonObject { ["id"] = SpoolSession.BlobPrefix + b.Sha256, ["name"] = b.Name, ["contentType"] = b.ContentType });
            }
        }
        var r = (JsonObject)result.DeepClone();
        AppendAttachments(r, refs);
        var entry = new JsonObject { ["v"] = SpoolSession.SpoolVersion, ["result"] = r, ["blobs"] = kept };
        if (!_spool.Append(entry))
        {
            Interlocked.Increment(ref _lost);
        }
    }

    private void Loop()
    {
        while (true)
        {
            lock (_lock)
            {
                if (_stop)
                {
                    return;
                }
                Monitor.Wait(_lock, _cfg.BatchInterval);
                if (_stop)
                {
                    return;
                }
            }
            try
            {
                Pump();
            }
            catch (Exception e)
            {
                WarnOnce("sender", $"reporting error (ignored): {e.Message}");
            }
        }
    }

    public void Pump()
    {
        lock (_sendLock)
        {
            if (Offline)
            {
                return;
            }
            if (LaunchId == null)
            {
                EnsureLaunch(0);
            }
            if (LaunchId == null)
            {
                return;
            }
            ProbeLogs();
            SendResults();
            SendLogs();
        }
    }

    private List<QueueItem> TakeBatch()
    {
        lock (_lock)
        {
            var batch = new List<QueueItem>();
            long size = 0;
            while (_queue.Count > 0 && batch.Count < ResultBatch)
            {
                var nxt = _queue[0];
                if (batch.Count > 0 && size + nxt.Bytes > ResultBatchBytes)
                {
                    break;
                }
                batch.Add(nxt);
                _queue.RemoveAt(0);
                size += nxt.Bytes;
            }
            return batch;
        }
    }

    private void Requeue(IEnumerable<QueueItem> items)
    {
        lock (_lock)
        {
            _queue.InsertRange(0, items);
        }
    }

    private void SendResults()
    {
        while (!Offline)
        {
            var batch = TakeBatch();
            if (batch.Count == 0)
            {
                return;
            }
            (int Accepted, int Duplicates, List<(int Index, string Message)> Rejected) summary;
            try
            {
                summary = _api.PostResults(LaunchId ?? string.Empty, batch.Select(b => (JsonNode)b.Result), 0);
            }
            catch (ApiException e)
            {
                if (e.Status == 413 && batch.Count > 1)
                {
                    foreach (var b in batch)
                    {
                        b.Bytes = ResultBatchBytes;
                    }
                    Requeue(batch);
                    continue;
                }
                if (e.Status == 413)
                {
                    Interlocked.Add(ref _lost, batch.Count(b => !b.Running));
                    WarnOnce("413", $"a result is too large for the server: {e.Message}");
                    continue;
                }
                Requeue(batch.Where(b => !b.Running));
                Failed(e);
                return;
            }
            Interlocked.Exchange(ref _failures, 0);
            var rejected = new HashSet<int>();
            foreach (var (idx, msg) in summary.Rejected)
            {
                rejected.Add(idx);
                if (idx >= 0 && idx < batch.Count && batch[idx].Running)
                {
                    Running = false;
                }
                else
                {
                    Interlocked.Increment(ref _lost);
                    WarnOnce($"rej:{(msg.Length > 40 ? msg.Substring(0, 40) : msg)}", $"the server rejected a result: {msg}");
                }
            }
            Interlocked.Add(ref _sent, batch.Where((b, i) => !b.Running && !rejected.Contains(i)).Count());
        }
    }

    private void SendLogs()
    {
        while (!Offline && LogsState != "off")
        {
            List<JsonObject> batch;
            lock (_lock)
            {
                batch = _logs.Take(1000).ToList();
                _logs.RemoveRange(0, batch.Count);
            }
            if (batch.Count == 0)
            {
                return;
            }
            string r;
            try
            {
                r = _api.PostLogs(LaunchId ?? string.Empty, batch, 0);
            }
            catch (ApiException e)
            {
                if (e.Fatal)
                {
                    WarnOnce("logs", $"live logs rejected ({e.Message}); they stay in the results");
                    LogsState = "off";
                    lock (_lock)
                    {
                        _logs = new List<JsonObject>();
                    }
                    return;
                }
                lock (_lock)
                {
                    _logs.InsertRange(0, batch);
                }
                Failed(e);
                return;
            }
            if (r == "unsupported")
            {
                LogsState = "off";
                lock (_lock)
                {
                    _logs = new List<JsonObject>();
                }
                return;
            }
            LogsState = "on";
            Interlocked.Exchange(ref _failures, 0);
        }
    }

    private void SpoolInflight()
    {
        List<Inflight> inflight;
        lock (_lock)
        {
            inflight = _inflight.Values.ToList();
            _inflight.Clear();
        }
        foreach (var f in inflight)
        {
            FoldLogs(f.Result, f.Fallback);
            var blobs = f.Files.Select(Prepare).OfType<PreparedFile>().ToList();
            SpoolResult(f.Result, blobs, f.Passed);
        }
    }

    /// <summary>Drains uploads and queues within the flush timeout, spools the rest, finishes the launch when
    /// asked, and returns a summary. Never throws.</summary>
    public RunSummary Close(string status = "finished")
    {
        var deadline = DateTime.UtcNow + _cfg.FlushTimeout;
        TimeSpan Remaining() => deadline > DateTime.UtcNow ? deadline - DateTime.UtcNow : TimeSpan.Zero;
        try
        {
            lock (_lock)
            {
                _stop = true;
                Monitor.PulseAll(_lock);
            }
            _thread?.Join(Remaining());
            _jobs.CompleteAdding();
            lock (_lock)
            {
                while (_inflight.Count > 0 && Remaining() > TimeSpan.Zero)
                {
                    Monitor.Wait(_lock, TimeSpan.FromMilliseconds(Math.Min(100, Remaining().TotalMilliseconds)));
                }
            }
            while (!Offline && Remaining() > TimeSpan.Zero)
            {
                int before;
                lock (_lock)
                {
                    before = _queue.Count + _logs.Count;
                }
                if (before == 0)
                {
                    break;
                }
                Pump();
                int after;
                lock (_lock)
                {
                    after = _queue.Count + _logs.Count;
                }
                if (after >= before)
                {
                    Thread.Sleep(Remaining() < TimeSpan.FromMilliseconds(200) ? Remaining() : TimeSpan.FromMilliseconds(200));
                }
            }
            _oldSpool?.Join(Remaining());
            bool leftover;
            lock (_lock)
            {
                leftover = _inflight.Count > 0 || _queue.Any(q => !q.Running);
            }
            if (leftover)
            {
                GoOffline(OfflineReason.Length > 0 ? OfflineReason : "the flush timeout was reached");
                SpoolInflight();
                List<QueueItem> queued;
                lock (_lock)
                {
                    queued = _queue.ToList();
                    _queue.Clear();
                }
                foreach (var q in queued.Where(q => !q.Running))
                {
                    SpoolResult(q.Result, new List<PreparedFile>(), true);
                }
            }
            var wantFinish = _cfg.FinishLaunch == "always" || (_cfg.FinishLaunch == "auto" && _cfg.OwnLaunch);
            if (_spool.Opened)
            {
                _spool.End(wantFinish ? status : null);
                if (!_cfg.Offline && Remaining() > TimeSpan.Zero)
                {
                    try
                    {
                        var r = SpoolFlusher.Flush(_api, _cfg.SpoolDir, null, deadline, _cfg.Project);
                        Interlocked.Add(ref _sent, r.Results);
                    }
                    catch (Exception)
                    {
                    }
                }
            }
            else if (wantFinish && LaunchId != null && Remaining() > TimeSpan.Zero)
            {
                try
                {
                    _api.FinishLaunch(LaunchId, status, 1);
                }
                catch (ApiException e)
                {
                    Failed(e);
                }
            }
        }
        catch (Exception e)
        {
            WarnOnce("close", $"reporting error (ignored): {e.Message}");
        }
        var left = _spool.Opened && Directory.Exists(_spool.Dir);
        return new RunSummary
        {
            LaunchId = LaunchId,
            LaunchNumber = LaunchNumber,
            Sent = _sent,
            Spooled = left ? _spool.WrittenResults : 0,
            Lost = _lost + _spool.DroppedResults,
            Attachments = _attachments,
            Offline = Offline,
            OfflineReason = OfflineReason,
            SpoolDir = left ? _spool.Dir : null,
            Logs = LogsState,
        };
    }
}
