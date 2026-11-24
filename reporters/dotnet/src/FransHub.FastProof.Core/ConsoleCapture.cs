using System;
using System.IO;
using System.Text;

namespace FransHub.FastProof;

internal static class ConsoleCapture
{
    private static readonly object Gate = new();
    private static bool s_installed;

    public static void Install()
    {
        lock (Gate)
        {
            if (s_installed)
            {
                return;
            }
            s_installed = true;
            Console.SetOut(new TeeWriter(Console.Out, false));
            Console.SetError(new TeeWriter(Console.Error, true));
        }
    }

    public static void FlushPartial(Attempt a)
    {
        string? o = null, e = null;
        lock (a.Lock)
        {
            if (a.PartialOut.Length > 0)
            {
                o = a.PartialOut.ToString();
                a.PartialOut.Clear();
            }
            if (a.PartialErr.Length > 0)
            {
                e = a.PartialErr.ToString();
                a.PartialErr.Clear();
            }
        }
        var s = ReporterSession.Current;
        if (s == null)
        {
            return;
        }
        if (o != null)
        {
            a.Stdout.Append(o);
            s.LogLine(a, "info", "stdout", o);
        }
        if (e != null)
        {
            a.Stderr.Append(e);
            s.LogLine(a, "warn", "stderr", e);
        }
    }

    private sealed class TeeWriter : TextWriter
    {
        private readonly TextWriter _inner;
        private readonly bool _error;

        public TeeWriter(TextWriter inner, bool error)
        {
            _inner = inner;
            _error = error;
        }

        public override Encoding Encoding => _inner.Encoding;

        public override IFormatProvider FormatProvider => _inner.FormatProvider;

        public override void Write(char value)
        {
            _inner.Write(value);
            Capture(value.ToString());
        }

        public override void Write(char[] buffer, int index, int count)
        {
            _inner.Write(buffer, index, count);
            Capture(new string(buffer, index, count));
        }

        public override void Write(string? value)
        {
            _inner.Write(value);
            if (value != null)
            {
                Capture(value);
            }
        }

        public override void WriteLine(string? value)
        {
            _inner.WriteLine(value);
            Capture((value ?? string.Empty) + "\n");
        }

        public override void WriteLine()
        {
            _inner.WriteLine();
            Capture("\n");
        }

        public override void Flush() => _inner.Flush();

        private void Capture(string text)
        {
            var a = Attempt.Current;
            var s = ReporterSession.Current;
            if (a == null || s == null || a.Ended || text.Length == 0)
            {
                return;
            }
            try
            {
                text = text.Replace("\r\n", "\n");
                var buf = _error ? a.PartialErr : a.PartialOut;
                string? complete = null;
                lock (a.Lock)
                {
                    buf.Append(text);
                    var all = buf.ToString();
                    var cut = all.LastIndexOf('\n');
                    if (cut >= 0)
                    {
                        complete = all.Substring(0, cut + 1);
                        buf.Clear();
                        buf.Append(all, cut + 1, all.Length - cut - 1);
                    }
                }
                if (complete == null)
                {
                    return;
                }
                (_error ? a.Stderr : a.Stdout).Append(complete);
                foreach (var line in complete.TrimEnd('\n').Split('\n'))
                {
                    s.LogLine(a, _error ? "warn" : "info", _error ? "stderr" : "stdout", line);
                }
            }
            catch (Exception)
            {
            }
        }
    }
}
