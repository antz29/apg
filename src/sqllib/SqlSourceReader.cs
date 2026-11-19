using System;
using System.Collections.Generic;
using System.IO;
using System.Text;
using System.Text.RegularExpressions;
using Microsoft.SqlServer.TransactSql.ScriptDom;

namespace Apg.SqlFrontend
{
    // Pure (path, text) reader: SQLCMD neutralisation, GO batch splitting, per-batch parsing. No I/O.
    public static class SqlSourceReader
    {
        public sealed record Batch(string Text, int StartLine, int StartColumn, int StartOffset);

        public sealed record ParsedBatch(Batch Batch, TSqlFragment Fragment);

        public sealed record Diagnostic(string Path, int Line, int Column, string Message)
        {
            public override string ToString() => FormatDiagnostic(Path, Line, Column, Message);
        }

        public sealed record Result(List<ParsedBatch> Batches, List<Diagnostic> Diagnostics);

        static readonly Regex SqlcmdVar = new Regex(@"\$\(([^()\s]+)\)", RegexOptions.CultureInvariant);
        static readonly Regex GoLine = new Regex(@"^\s*GO(\s+\d+)?\s*(--.*)?$",
            RegexOptions.IgnoreCase | RegexOptions.CultureInvariant);

        public static string FormatDiagnostic(string path, int line, int col, string message) =>
            $"sql: {path}:{line}:{col}: {message}";

        public static Result Read(string path, string text)
        {
            var parsed = new List<ParsedBatch>();
            var diagnostics = new List<Diagnostic>();
            foreach (var batch in SplitBatches(Neutralise(text)))
            {
                var fragment = new TSql170Parser(true).Parse(new StringReader(batch.Text), out IList<ParseError> errors);
                if (errors.Count == 0)
                {
                    parsed.Add(new ParsedBatch(batch, fragment));
                    continue;
                }
                foreach (var e in errors)
                    diagnostics.Add(new Diagnostic(path, batch.StartLine + e.Line - 1,
                        e.Line == 1 ? batch.StartColumn + e.Column - 1 : e.Column, e.Message));
            }
            return new Result(parsed, diagnostics);
        }

        // Same length, same line breaks: SQLCMD command lines become blanks, $(var) a same-length identifier.
        public static string Neutralise(string text)
        {
            var sb = new StringBuilder(text.Length);
            foreach (var (start, end, next) in Lines(text))
            {
                var line = text.Substring(start, end - start);
                if (line.TrimStart().StartsWith(":"))
                    sb.Append(' ', line.Length);
                else
                    sb.Append(SqlcmdVar.Replace(line, m => "__" + Regex.Replace(m.Groups[1].Value, @"[^\w]", "_") + "_"));
                sb.Append(text, end, next - end);
            }
            return sb.ToString();
        }

        // A GO line outside a block comment, string or quoted identifier ends a batch; the GO line itself is dropped.
        public static List<Batch> SplitBatches(string text)
        {
            var batches = new List<Batch>();
            int batchStart = 0, batchLine = 1, lineNo = 0;
            char state = '\0'; // '\0' code, '*' block comment, '\'' string, '"' quoted id, '[' bracket id
            int depth = 0;
            foreach (var (start, end, next) in Lines(text))
            {
                lineNo++;
                var line = text.Substring(start, end - start);
                if (state == '\0' && GoLine.IsMatch(line))
                {
                    batches.Add(new Batch(text.Substring(batchStart, start - batchStart), batchLine, 1, batchStart));
                    batchStart = next;
                    batchLine = lineNo + 1;
                    continue;
                }
                for (int i = 0; i < line.Length; i++)
                {
                    char c = line[i], n = i + 1 < line.Length ? line[i + 1] : '\0';
                    switch (state)
                    {
                        case '\0':
                            if (c == '-' && n == '-') i = line.Length;
                            else if (c == '/' && n == '*') { state = '*'; depth = 1; i++; }
                            else if (c == '\'' || c == '"' || c == '[') state = c;
                            break;
                        case '*': // T-SQL block comments nest
                            if (c == '/' && n == '*') { depth++; i++; }
                            else if (c == '*' && n == '/') { i++; if (--depth == 0) state = '\0'; }
                            break;
                        case '[':
                            if (c == ']') { if (n == ']') i++; else state = '\0'; }
                            break;
                        default: // '' and "" escapes re-enter on the next quote
                            if (c == state) state = '\0';
                            break;
                    }
                }
            }
            batches.Add(new Batch(text.Substring(batchStart), batchLine, 1, batchStart));
            return batches;
        }

        // (content start, content end excluding \r\n or \n, start of next line) for every line.
        static IEnumerable<(int, int, int)> Lines(string text)
        {
            int start = 0;
            while (true)
            {
                int nl = text.IndexOf('\n', start);
                if (nl < 0)
                {
                    yield return (start, text.Length, text.Length);
                    yield break;
                }
                yield return (start, nl > start && text[nl - 1] == '\r' ? nl - 1 : nl, nl + 1);
                start = nl + 1;
            }
        }
    }
}
