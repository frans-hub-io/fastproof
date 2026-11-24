using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Threading;

namespace FransHub.FastProof;

internal sealed class ApiException : Exception
{
    public ApiException(string message, int status, bool fatal) : base(message)
    {
        Status = status;
        Fatal = fatal;
    }

    public int Status { get; }

    public bool Fatal { get; }
}

internal sealed class Response
{
    public int Status;
    public JsonNode? Body;
    public double? RetryAfter;
    public string Error = string.Empty;

    public bool Ok => Status is >= 200 and < 300;

    public bool Retryable => Status == 0 || Status is 408 or 429 || Status >= 500;

    public string Describe()
    {
        if (Status == 0)
        {
            return $"server unreachable ({(Error.Length > 0 ? Error : "network error")})";
        }
        if (Status < 0)
        {
            return Error.Length > 0 ? Error : "local error";
        }
        string? msg = null;
        if (Body is JsonObject o && o["message"] is JsonValue v && v.TryGetValue<string>(out var s))
        {
            msg = s;
        }
        return $"HTTP {Status}" + (msg != null ? $": {msg}" : string.Empty);
    }
}

internal sealed class Api
{
    public const string Version = "0.1.0";
    public static readonly string UserAgent = $"fastproof-dotnet/{Version}";
    private static readonly JsonSerializerOptions JsonOptions = new() { Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping };

    private readonly HttpClient _http;
    private readonly HttpClient _plain;
    private readonly string _base;
    private readonly string _token;
    private readonly string _project;
    private readonly TimeSpan _timeout;
    private readonly int _retries;
    private readonly Random _random = new();

    public Api(string url, string token, string project, TimeSpan timeout, int retries = 4)
    {
        _base = url.TrimEnd('/');
        _token = token;
        _project = project;
        _timeout = timeout;
        _retries = retries;
        _http = NewClient();
        _plain = NewClient();
    }

    private static HttpClient NewClient() => new(new SocketsHttpHandler { AllowAutoRedirect = false, UseCookies = false })
    {
        Timeout = Timeout.InfiniteTimeSpan,
    };

    public static string Serialize(JsonNode node) => node.ToJsonString(JsonOptions);

    public static byte[] Ndjson(IEnumerable<JsonNode> items)
    {
        var sb = new StringBuilder();
        foreach (var i in items)
        {
            sb.Append(Serialize(i)).Append('\n');
        }
        return Encoding.UTF8.GetBytes(sb.ToString());
    }

    public static string EncodeHeader(string v)
    {
        var s = v.Replace('\r', ' ').Replace('\n', ' ');
        if (s.Length > 255)
        {
            s = s.Substring(0, 255);
        }
        return s.All(c => c >= 0x20 && c <= 0x7E) ? s : Uri.EscapeDataString(s);
    }

    public double Backoff(int attempt, double baseSeconds = 0.25, double cap = 8.0)
    {
        var exp = Math.Min(cap, baseSeconds * Math.Pow(2, Math.Min(attempt, 16)));
        double r;
        lock (_random)
        {
            r = _random.NextDouble();
        }
        return exp / 2 + r * exp / 2;
    }

    private string PathOf(string? launchId = null)
    {
        var p = $"/api/v1/projects/{Uri.EscapeDataString(_project)}";
        return launchId != null ? $"{p}/launches/{Uri.EscapeDataString(launchId)}" : p;
    }

    private static JsonNode? Parse(byte[] raw)
    {
        if (raw.Length == 0)
        {
            return null;
        }
        try
        {
            return JsonNode.Parse(raw);
        }
        catch (JsonException)
        {
            return JsonValue.Create(Encoding.UTF8.GetString(raw));
        }
    }

    private static double? ParseRetryAfter(HttpResponseMessage r)
    {
        var ra = r.Headers.RetryAfter;
        if (ra == null)
        {
            return null;
        }
        double? v = ra.Delta?.TotalSeconds ?? (ra.Date.HasValue ? (ra.Date.Value - DateTimeOffset.UtcNow).TotalSeconds : null);
        return v.HasValue ? Math.Min(Math.Max(v.Value, 0), 60) : null;
    }

    public Response Request(HttpMethod method, string path, byte[]? data = null, string? contentType = null,
        IDictionary<string, string>? headers = null, TimeSpan? timeout = null, string? file = null, Func<Stream>? source = null)
    {
        Stream? fs = null;
        try
        {
            using var req = new HttpRequestMessage(method, _base + path);
            req.Headers.TryAddWithoutValidation("Accept", "application/json");
            req.Headers.TryAddWithoutValidation("User-Agent", UserAgent);
            if (_token.Length > 0)
            {
                req.Headers.Authorization = new AuthenticationHeaderValue("Bearer", _token);
            }
            foreach (var kv in headers ?? new Dictionary<string, string>())
            {
                req.Headers.TryAddWithoutValidation(kv.Key, kv.Value);
            }
            if (file != null || source != null)
            {
                try
                {
                    fs = source != null ? source() : new FileStream(file!, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                }
                catch (Exception e) when (e is IOException or UnauthorizedAccessException)
                {
                    return new Response { Status = -1, Error = $"cannot read {file ?? "the attachment"}: {e.Message}" };
                }
                req.Content = new StreamContent(fs);
            }
            else if (data != null)
            {
                req.Content = new ByteArrayContent(data);
            }
            if (req.Content != null && contentType != null)
            {
                req.Content.Headers.ContentType = MediaTypeHeaderValue.Parse(contentType);
            }
            using var cts = new CancellationTokenSource(timeout ?? _timeout);
            using var res = _http.Send(req, HttpCompletionOption.ResponseContentRead, cts.Token);
            using var ms = new MemoryStream();
            res.Content.CopyTo(ms, null, cts.Token);
            return new Response { Status = (int)res.StatusCode, Body = Parse(ms.ToArray()), RetryAfter = res.IsSuccessStatusCode ? null : ParseRetryAfter(res) };
        }
        catch (Exception e) when (e is HttpRequestException or IOException or OperationCanceledException or InvalidOperationException or FormatException)
        {
            return new Response { Status = 0, Error = e.InnerException?.Message ?? e.Message };
        }
        finally
        {
            fs?.Dispose();
        }
    }

    public Response Call(Func<Response> fn, int? retries = null)
    {
        var n = retries ?? _retries;
        var last = new Response();
        for (var i = 0; i <= n; i++)
        {
            last = fn();
            if (last.Ok)
            {
                return last;
            }
            if (!last.Retryable)
            {
                throw new ApiException(last.Describe(), last.Status, true);
            }
            if (i < n)
            {
                Thread.Sleep(TimeSpan.FromSeconds(last.RetryAfter ?? Backoff(i)));
            }
        }
        throw new ApiException(last.Describe(), last.Status, false);
    }

    public JsonObject StartLaunch(JsonObject start, int? retries = null)
    {
        var body = Encoding.UTF8.GetBytes(Serialize(start));
        var r = Call(() => Request(HttpMethod.Post, $"{PathOf()}/launches", body, "application/json"), retries);
        if (r.Body is not JsonObject o || o["id"] is not JsonValue)
        {
            throw new ApiException("unexpected launch response", r.Status, true);
        }
        return o;
    }

    public (int Accepted, int Duplicates, List<(int Index, string Message)> Rejected) PostResults(string launchId, IEnumerable<JsonNode> results, int? retries = null)
    {
        var body = Ndjson(results);
        var r = Call(() => Request(HttpMethod.Post, $"{PathOf(launchId)}/results", body, "application/x-ndjson"), retries);
        var b = r.Body as JsonObject;
        var rejected = new List<(int, string)>();
        if (b?["rejected"] is JsonArray arr)
        {
            foreach (var x in arr)
            {
                rejected.Add((x?["index"]?.GetValue<int>() ?? -1, x?["message"]?.GetValue<string>() ?? string.Empty));
            }
        }
        return (b?["accepted"]?.GetValue<int>() ?? 0, b?["duplicates"]?.GetValue<int>() ?? 0, rejected);
    }

    public string PostLogs(string launchId, IEnumerable<JsonNode> lines, int? retries = null)
    {
        var body = Ndjson(lines);
        try
        {
            Call(() => Request(HttpMethod.Post, $"{PathOf(launchId)}/logs", body, "application/x-ndjson"), retries);
            return "ok";
        }
        catch (ApiException e) when (e.Status is 404 or 405 or 501)
        {
            return "unsupported";
        }
    }

    public JsonObject UploadAttachment(string launchId, string name, string contentType, string? path = null, byte[]? body = null, int? retries = null,
        Func<Stream>? source = null)
    {
        var headers = new Dictionary<string, string> { ["X-FastProof-Attachment-Name"] = EncodeHeader(name) };
        var r = Call(() => Request(HttpMethod.Post, $"{PathOf(launchId)}/attachments", path == null && source == null ? body : null, contentType, headers,
            TimeSpan.FromSeconds(120), path, source), retries);
        if (r.Body is not JsonObject o || o["id"] is not JsonValue)
        {
            throw new ApiException("unexpected attachment response", r.Status, true);
        }
        return o;
    }

    public JsonObject DirectUpload(string launchId, string sha256, long size, string name, string contentType, int? retries = null)
    {
        var req = new JsonObject
        {
            ["items"] = new JsonArray(new JsonObject { ["sha256"] = sha256, ["size"] = size, ["name"] = name, ["contentType"] = contentType }),
        };
        var body = Encoding.UTF8.GetBytes(Serialize(req));
        var r = Call(() => Request(HttpMethod.Post, $"{PathOf(launchId)}/attachments/direct", body, "application/json"), retries);
        if ((r.Body as JsonObject)?["items"] is JsonArray { Count: > 0 } items && items[0] is JsonObject first && first["status"] is JsonValue)
        {
            return first;
        }
        throw new ApiException("unexpected direct upload response", r.Status, true);
    }

    public string CompleteDirect(string launchId, string uploadId, int? retries = null)
    {
        var path = $"{PathOf(launchId)}/attachments/direct/{Uri.EscapeDataString(uploadId)}/complete";
        ApiException? last = null;
        for (var i = 0; i < 4; i++)
        {
            Response r;
            try
            {
                r = Call(() => Request(HttpMethod.Post, path, timeout: TimeSpan.FromSeconds(120)), retries);
            }
            catch (ApiException e) when (e.Status == 409)
            {
                last = e;
                Thread.Sleep(TimeSpan.FromSeconds(Backoff(i, 0.5)));
                continue;
            }
            if (r.Body is not JsonObject o || o["id"]?.GetValue<string>() is not { } id)
            {
                throw new ApiException("unexpected completion response", r.Status, true);
            }
            return id;
        }
        throw last!;
    }

    public void FinishLaunch(string launchId, string status, int? retries = null)
    {
        var body = Encoding.UTF8.GetBytes(Serialize(new JsonObject { ["status"] = status }));
        Call(() => Request(HttpMethod.Post, $"{PathOf(launchId)}/finish", body, "application/json"), retries);
    }

    public int PutPresigned(string url, IDictionary<string, string> headers, long size, string? path, byte[]? body, TimeSpan timeout)
    {
        FileStream? fs = null;
        try
        {
            using var req = new HttpRequestMessage(HttpMethod.Put, url);
            if (path != null)
            {
                fs = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                req.Content = new StreamContent(fs);
            }
            else
            {
                req.Content = new ByteArrayContent(body ?? Array.Empty<byte>());
            }
            foreach (var kv in headers)
            {
                if (string.Equals(kv.Key, "content-length", StringComparison.OrdinalIgnoreCase))
                {
                    continue;
                }
                if (!req.Headers.TryAddWithoutValidation(kv.Key, kv.Value))
                {
                    req.Content.Headers.Remove(kv.Key);
                    req.Content.Headers.TryAddWithoutValidation(kv.Key, kv.Value);
                }
            }
            req.Content.Headers.ContentLength = size;
            using var cts = new CancellationTokenSource(timeout);
            using var res = _plain.Send(req, HttpCompletionOption.ResponseHeadersRead, cts.Token);
            return (int)res.StatusCode;
        }
        catch (Exception e) when (e is HttpRequestException or IOException or OperationCanceledException or UnauthorizedAccessException or InvalidOperationException or FormatException)
        {
            return 0;
        }
        finally
        {
            fs?.Dispose();
        }
    }
}
