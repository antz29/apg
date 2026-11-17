using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.Json;
using Microsoft.SqlServer.TransactSql.ScriptDom;

namespace Apg.SqlFrontend
{
    // Pure unified-JSONL emitter over database scopes and in-memory .sql texts. No I/O.
    public static class SchemaEmitter
    {
        // rootDir: absolute, '/'-separated, no trailing separator. sources: repo-relative .sql path -> text.
        public static void Emit(string rootDir, List<SqlProjectReader.Scope> scopes,
            IReadOnlyDictionary<string, string> sources, string idPrefix, TextWriter stdout, TextWriter stderr)
        {
            new Run(rootDir, idPrefix, stdout).All(scopes, sources, stderr);
        }

        sealed class Run
        {
            readonly string root, prefix;
            readonly TextWriter output;
            readonly HashSet<string> modules = new HashSet<string>(StringComparer.Ordinal);
            int next;

            public Run(string rootDir, string idPrefix, TextWriter stdout)
            {
                root = rootDir; prefix = idPrefix; output = stdout;
            }

            void Write(object record) => output.WriteLine(JsonSerializer.Serialize(record));

            bool Module(string fqn)
            {
                if (!modules.Add(fqn)) return false;
                Write(new { type = "module", fqn });
                return true;
            }

            string ScopeModule(SqlProjectReader.Scope s) =>
                s.ModuleIdentity.Length == 0 ? root : root + "/" + s.ModuleIdentity;

            public void All(List<SqlProjectReader.Scope> scopes, IReadOnlyDictionary<string, string> sources, TextWriter stderr)
            {
                var owner = new Dictionary<string, SqlProjectReader.Scope>(StringComparer.Ordinal);
                foreach (var s in scopes)
                {
                    Module(ScopeModule(s));
                    foreach (var f in s.Files) owner[f] = s;
                }
                foreach (var path in owner.Keys.OrderBy(p => p, StringComparer.Ordinal))
                {
                    if (!sources.TryGetValue(path, out var text)) continue;
                    var result = SqlSourceReader.Read(path, text);
                    foreach (var d in result.Diagnostics) stderr.WriteLine(d);
                    EmitFile(owner[path], path, text, result.Batches);
                }
            }

            void EmitFile(SqlProjectReader.Scope scope, string rel, string text, List<SqlSourceReader.ParsedBatch> batches)
            {
                string abs = root + "/" + rel, scopeModule = ScopeModule(scope);
                var map = new Positions(text);
                Write(new { type = "file", path = abs, parent = scopeModule, start_line = 1, end_line = map.LineCount });
                foreach (var pb in batches)
                {
                    if (pb.Fragment is not TSqlScript script) continue;
                    foreach (var stmt in script.Batches.SelectMany(b => b.Statements))
                    {
                        var decl = Declare(stmt, scope.DefaultSchema);
                        if (decl == null) continue;
                        var (schema, name, isFunction, columns) = decl.Value;
                        string parent = scope.IsProject ? scopeModule + "." + schema : schema;
                        if (Module(parent) && scope.IsProject)
                            Write(new { type = "contains", from = scopeModule, to = parent });
                        string id = prefix + (++next);
                        var (start, end, sl, el) = map.Span(pb.Batch, stmt);
                        if (isFunction)
                            Write(new { type = "function", id, parent, name, @params = new string[0], file = abs, path = abs,
                                start, end, start_line = sl, end_line = el });
                        else
                            Write(new { type = "struct", id, parent, name, path = abs, start, end, start_line = sl, end_line = el });
                        foreach (var (column, fragment) in columns)
                        {
                            string cid = prefix + (++next);
                            var (cs, ce, csl, cel) = map.Span(pb.Batch, fragment);
                            Write(new { type = "struct", id = cid, parent = parent + "." + name, name = column, path = abs,
                                start = cs, end = ce, start_line = csl, end_line = cel });
                            Write(new { type = "contains", from = id, to = cid });
                        }
                    }
                }
            }
        }

        static readonly List<(string, TSqlFragment)> NoColumns = new List<(string, TSqlFragment)>();

        // (schema, name, is function, columns) of a declaring statement; null for every other statement.
        static (string, string, bool, List<(string, TSqlFragment)>)? Declare(TSqlStatement stmt, string defaultSchema)
        {
            switch (stmt)
            {
                case ViewStatementBody v:
                    return (Schema(v.SchemaObjectName, defaultSchema), v.SchemaObjectName.BaseIdentifier.Value, false, ViewColumns(v));
                case ProcedureStatementBody p:
                    return (Schema(p.ProcedureReference.Name, defaultSchema), p.ProcedureReference.Name.BaseIdentifier.Value, true, NoColumns);
                case FunctionStatementBody f:
                    return (Schema(f.Name, defaultSchema), f.Name.BaseIdentifier.Value, true, NoColumns);
                case TriggerStatementBody t:
                    // A one-part DML trigger lives in its table's schema; a DDL trigger in the file's default schema.
                    string triggerSchema = t.TriggerObject.TriggerScope == TriggerScope.Normal
                        ? Schema(t.TriggerObject.Name, defaultSchema) : defaultSchema;
                    return (Schema(t.Name, triggerSchema), t.Name.BaseIdentifier.Value, true, NoColumns);
                case CreateTableStatement c:
                    return (Schema(c.SchemaObjectName, defaultSchema), c.SchemaObjectName.BaseIdentifier.Value, false,
                        Columns(c.Definition?.ColumnDefinitions));
                case CreateTypeTableStatement tt:
                    return (Schema(tt.Name, defaultSchema), tt.Name.BaseIdentifier.Value, false, Columns(tt.Definition?.ColumnDefinitions));
                case CreateTypeStatement ty:
                    return (Schema(ty.Name, defaultSchema), ty.Name.BaseIdentifier.Value, false, NoColumns);
                case CreateSequenceStatement s:
                    return (Schema(s.Name, defaultSchema), s.Name.BaseIdentifier.Value, false, NoColumns);
                case CreateSynonymStatement s:
                    return (Schema(s.Name, defaultSchema), s.Name.BaseIdentifier.Value, false, NoColumns);
                default:
                    return null;
            }
        }

        static string Schema(SchemaObjectName name, string fallback) => name.SchemaIdentifier?.Value ?? fallback;

        static List<(string, TSqlFragment)> Columns(IList<ColumnDefinition> defs) =>
            defs == null ? NoColumns : defs.Select(d => (d.ColumnIdentifier.Value, (TSqlFragment)d)).ToList();

        // The view's column list, else each named select-list output of its first query specification.
        static List<(string, TSqlFragment)> ViewColumns(ViewStatementBody v)
        {
            if (v.Columns != null && v.Columns.Count > 0)
                return v.Columns.Select(c => (c.Value, (TSqlFragment)c)).ToList();
            var query = v.SelectStatement?.QueryExpression;
            while (true)
            {
                if (query is BinaryQueryExpression b) query = b.FirstQueryExpression;
                else if (query is QueryParenthesisExpression p) query = p.QueryExpression;
                else break;
            }
            if (query is not QuerySpecification spec) return NoColumns;
            var columns = new List<(string, TSqlFragment)>();
            foreach (var element in spec.SelectElements.OfType<SelectScalarExpression>())
            {
                string name = element.ColumnName?.Value
                    ?? (element.Expression as ColumnReferenceExpression)?.MultiPartIdentifier?.Identifiers.LastOrDefault()?.Value;
                if (name != null) columns.Add((name, element));
            }
            return columns;
        }

        // Original-file positions: batch-relative fragment offsets shifted by the batch's UTF-16 start, then mapped to lines and UTF-8 bytes.
        sealed class Positions
        {
            readonly int[] bytes;
            readonly List<int> newlines = new List<int>();

            public Positions(string text)
            {
                bytes = new int[text.Length + 1];
                for (int i = 0; i < text.Length; i++)
                {
                    char c = text[i];
                    bytes[i + 1] = bytes[i] + (c < 0x80 ? 1 : c < 0x800 || char.IsSurrogate(c) ? 2 : 3);
                    if (c == '\n') newlines.Add(i);
                }
                // A trailing newline ends the last line rather than opening an empty one; an empty file is one line.
                LineCount = Math.Max(1, newlines.Count + (text.Length > 0 && text[^1] != '\n' ? 1 : 0));
            }

            public int LineCount { get; }

            int Line(int offset)
            {
                int i = newlines.BinarySearch(offset);
                return (i < 0 ? ~i : i) + 1;
            }

            public (int, int, int, int) Span(SqlSourceReader.Batch batch, TSqlFragment fragment)
            {
                int start = batch.StartOffset + fragment.StartOffset;
                int end = start + fragment.FragmentLength;
                return (bytes[start], bytes[end], Line(start), Line(Math.Max(start, end - 1)));
            }
        }
    }
}
