using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;
using System.Threading;

namespace FransHub.FastProof.Tests;

public sealed class FakeServer : IDisposable
{
    private readonly HttpListener _listener = new();
    private readonly Thread _thread;
    private int _launchCounter;

    public FakeServer()
    {
        var port = FreePort();
        Url = $"http://127.0.0.1:{port}";
        _listener.Prefixes.Add(Url + "/");
        _listener.Start();
        _thread = new Thread(Loop) { IsBackground = true };
        _thread.Start();
    }

    public string Url { get; }
    public ConcurrentQueue<JsonObject> Results { get; } = new();
    public ConcurrentQueue<JsonObject> Logs { get; } = new();
    public ConcurrentDictionary<string, byte[]> Attachments { get; } = new();
    public ConcurrentQueue<string> Launches { get; } = new();
    public ConcurrentQueue<string> Finished { get; } = new();
    public int AttachmentUploads;
    public bool RejectRunning;
    public bool NoLogs;
    public int FailWith;
    public string? ExpectToken;

    public static int FreePort()
    {
        var l = new System.Net.Sockets.TcpListener(IPAddress.Loopback, 0);
        l.Start();
        var port = ((IPEndPoint)l.LocalEndpoint).Port;
        l.Stop();
        return port;
    }

    private void Loop()
    {
        while (_listener.IsListening)
        {
            HttpListenerContext ctx;
            try
            {
                ctx = _listener.GetContext();
            }
            catch (Exception)
            {
                return;
            }
            ThreadPool.QueueUserWorkItem(_ => Handle(ctx));
        }
    }

    private static void Reply(HttpListenerContext ctx, int status, JsonNode? body = null)
    {
        ctx.Response.StatusCode = status;
        if (body != null)
        {
            var bytes = Encoding.UTF8.GetBytes(body.ToJsonString());
            ctx.Response.ContentType = "application/json";
            ctx.Response.ContentLength64 = bytes.Length;
            ctx.Response.OutputStream.Write(bytes, 0, bytes.Length);
        }
        ctx.Response.Close();
    }

    private void Handle(HttpListenerContext ctx)
    {
        try
        {
            using var ms = new MemoryStream();
            ctx.Request.InputStream.CopyTo(ms);
            var body = ms.ToArray();
            var path = ctx.Request.Url!.AbsolutePath;
            if (FailWith != 0)
            {
                Reply(ctx, FailWith, new JsonObject { ["code"] = "down", ["message"] = "down" });
                return;
            }
            if (ExpectToken != null && ctx.Request.Headers["Authorization"] != $"Bearer {ExpectToken}")
            {
                Reply(ctx, 401, new JsonObject { ["code"] = "unauthorized", ["message"] = "no" });
                return;
            }
            if (path.EndsWith("/launches", StringComparison.Ordinal))
            {
                var o = JsonNode.Parse(body)!.AsObject();
                var ext = o["externalId"]!.GetValue<string>();
                Launches.Enqueue(ext);
                Reply(ctx, 201, new JsonObject { ["id"] = "launch-1", ["number"] = Interlocked.Increment(ref _launchCounter), ["externalId"] = ext });
            }
            else if (path.EndsWith("/results", StringComparison.Ordinal))
            {
                var rejected = new JsonArray();
                var i = 0;
                var accepted = 0;
                foreach (var line in Encoding.UTF8.GetString(body).Split('\n', StringSplitOptions.RemoveEmptyEntries))
                {
                    var r = JsonNode.Parse(line)!.AsObject();
                    if (RejectRunning && r["status"]?.GetValue<string>() == "running")
                    {
                        rejected.Add(new JsonObject { ["index"] = i, ["message"] = "unknown variant `running`" });
                    }
                    else
                    {
                        Results.Enqueue(r);
                        accepted++;
                    }
                    i++;
                }
                Reply(ctx, 202, new JsonObject { ["accepted"] = accepted, ["duplicates"] = 0, ["rejected"] = rejected });
            }
            else if (path.EndsWith("/logs", StringComparison.Ordinal))
            {
                if (NoLogs)
                {
                    Reply(ctx, 404, new JsonObject { ["code"] = "not_found", ["message"] = "no" });
                    return;
                }
                foreach (var line in Encoding.UTF8.GetString(body).Split('\n', StringSplitOptions.RemoveEmptyEntries))
                {
                    Logs.Enqueue(JsonNode.Parse(line)!.AsObject());
                }
                Reply(ctx, 202, new JsonObject { ["accepted"] = 0 });
            }
            else if (path.EndsWith("/attachments", StringComparison.Ordinal))
            {
                Interlocked.Increment(ref AttachmentUploads);
                var sha = Convert.ToHexString(SHA256.HashData(body)).ToLowerInvariant();
                var id = "att-" + sha.Substring(0, 12);
                Attachments[id] = body;
                Reply(ctx, 201, new JsonObject { ["id"] = id, ["sha256"] = sha, ["size"] = body.Length });
            }
            else if (path.EndsWith("/finish", StringComparison.Ordinal))
            {
                Finished.Enqueue(Encoding.UTF8.GetString(body));
                Reply(ctx, 200, new JsonObject());
            }
            else
            {
                Reply(ctx, 404, new JsonObject { ["code"] = "not_found", ["message"] = "no" });
            }
        }
        catch (Exception)
        {
            try
            {
                Reply(ctx, 500);
            }
            catch (Exception)
            {
            }
        }
    }

    public IEnumerable<JsonObject> Final() => Results.Where(r => r["status"]?.GetValue<string>() != "running");

    public IEnumerable<JsonObject> Running() => Results.Where(r => r["status"]?.GetValue<string>() == "running");

    public void Dispose()
    {
        _listener.Close();
    }
}
