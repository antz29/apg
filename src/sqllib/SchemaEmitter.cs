using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Xml;
using Microsoft.SqlServer.TransactSql.ScriptDom;

namespace Apg.SqlFrontend
{
    // Unified-JSONL emitter: Emit is pure over database scopes and in-memory .sql texts; Run reads the listed files.
    public static class SchemaEmitter
    {
        // One emitted object of a scope: canonical spelling Schema.Name, its id and Function (true) or Struct kind.
        public sealed record Declaration(string Schema, string Name, string Id, bool IsFunction);

        // rootDir: absolute, '/'-separated, no trailing separator. sources: repo-relative .sql path -> text.
        // Returns each scope's declaration index: Schema.Name (ordinal-ignore-case) -> emitted declaration.
        public static Dictionary<SqlProjectReader.Scope, Dictionary<string, Declaration>> Emit(string rootDir,
            List<SqlProjectReader.Scope> scopes, IReadOnlyDictionary<string, string> sources, string idPrefix,
            TextWriter stdout, TextWriter stderr)
        {
            var emission = new Emission(rootDir, idPrefix, stdout, stderr);
            emission.All(scopes, sources);
            return emission.Index;
        }

        // The scan entry over the --targets list: the listed absolute paths are the only input (no directory walk).
        // moduleDirs are absolute; returns Emit's declaration index.
        public static Dictionary<SqlProjectReader.Scope, Dictionary<string, Declaration>> Run(string rootDir,
            IEnumerable<string> targetPaths, IReadOnlyList<string> moduleDirs, IReadOnlyList<string> excludePatterns,
            string idPrefix, TextWriter stdout, TextWriter stderr)
        {
            var mods = moduleDirs.Select(m => Path.TrimEndingDirectorySeparator(Path.GetFullPath(m))).ToList();
            var seen = new HashSet<string>(StringComparer.Ordinal);
            var projects = new List<string>();
            var sources = new Dictionary<string, string>(StringComparer.Ordinal);
            foreach (var line in targetPaths)
            {
                string full = Path.GetFullPath(line);
                string rel = Path.GetRelativePath(rootDir, full).Replace(Path.DirectorySeparatorChar, '/');
                if (rel == ".." || rel.StartsWith("../", StringComparison.Ordinal) || Path.IsPathRooted(rel) || !seen.Add(rel))
                    continue;
                string ext = Path.GetExtension(full);
                bool isProject = ext.Equals(".sqlproj", StringComparison.OrdinalIgnoreCase);
                if (!isProject && !ext.Equals(".sql", StringComparison.OrdinalIgnoreCase)) continue;
                if (excludePatterns.Any(p => full.Contains(p, StringComparison.OrdinalIgnoreCase))) continue;
                if (mods.Count > 0 && !mods.Any(m => IsWithin(full, m))) continue;
                try
                {
                    // A project is parsed here too so one unreadable or malformed .sqlproj is reported, not scan-fatal.
                    if (isProject) { SqlProjectReader.ParseProject(rel, File.ReadAllText(full)); projects.Add(rel); }
                    else if (!File.Exists(full)) stderr.WriteLine($"sql: {rel}: file not found");
                    else sources[rel] = File.ReadAllText(full, Encoding.UTF8);
                }
                catch (Exception e) when (e is IOException || e is UnauthorizedAccessException || e is XmlException)
                {
                    stderr.WriteLine($"sql: {rel}: {(e is FileNotFoundException || e is DirectoryNotFoundException ? "file not found" : e.Message)}");
                }
            }
            var scopes = SqlProjectReader.Read(rootDir, projects, sources.Keys);
            return Emit(rootDir, scopes, sources, idPrefix, stdout, stderr);
        }

        // `path` equals `dir` or lies under it, on a separator boundary (as csharplib SourceDiscovery.IsWithin).
        static bool IsWithin(string path, string dir) =>
            path.StartsWith(dir, StringComparison.Ordinal)
            && (path.Length == dir.Length || path[dir.Length] == Path.DirectorySeparatorChar || dir.EndsWith(Path.DirectorySeparatorChar));

        sealed class Emission
        {
            readonly string root, prefix;
            readonly TextWriter output, errors;
            readonly HashSet<string> written = new HashSet<string>(StringComparer.Ordinal);
            // Repo-relative module identity (ignore-case) -> first spelling and its (path, offset) declaration point.
            readonly Dictionary<string, (string Fqn, string Path, int Offset)> modules =
                new Dictionary<string, (string, string, int)>(StringComparer.OrdinalIgnoreCase);
            // Repo-relative rendered FQN (ignore-case) -> what claimed it, for collision reports.
            readonly Dictionary<string, string> claimed = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
            readonly Dictionary<SqlProjectReader.Scope, Dictionary<string, string>> firsts =
                new Dictionary<SqlProjectReader.Scope, Dictionary<string, string>>();
            public readonly Dictionary<SqlProjectReader.Scope, Dictionary<string, Declaration>> Index =
                new Dictionary<SqlProjectReader.Scope, Dictionary<string, Declaration>>();
            int next;

            public Emission(string rootDir, string idPrefix, TextWriter stdout, TextWriter stderr)
            {
                root = rootDir; prefix = idPrefix; output = stdout; errors = stderr;
            }

            void Write(object record) => output.WriteLine(JsonSerializer.Serialize(record));

            string Rel(string identity) =>
                identity == root ? "" : identity.StartsWith(root + "/", StringComparison.Ordinal) ? identity.Substring(root.Length + 1) : identity;

            void Candidate(string fqn, string path, int offset)
            {
                string key = Rel(fqn);
                if (modules.TryGetValue(key, out var first))
                {
                    int cmp = string.CompareOrdinal(path, first.Path);
                    if (cmp > 0 || (cmp == 0 && offset >= first.Offset)) return;
                }
                modules[key] = (fqn, path, offset);
            }

            string Canonical(string fqn) => modules[Rel(fqn)].Fqn;

            void Module(string fqn)
            {
                if (written.Add("module\n" + fqn)) Write(new { type = "module", fqn });
            }

            string ScopeModule(SqlProjectReader.Scope s) =>
                s.ModuleIdentity.Length == 0 ? root : root + "/" + s.ModuleIdentity;

            string Declared(SqlProjectReader.Scope s, string schema) => s.IsProject ? ScopeModule(s) + "." + schema : schema;

            void Report(string rel, SqlSourceReader.Batch batch, TSqlFragment at, string message) =>
                errors.WriteLine(SqlSourceReader.FormatDiagnostic(rel, batch.StartLine + at.StartLine - 1,
                    at.StartLine == 1 ? batch.StartColumn + at.StartColumn - 1 : at.StartColumn, message));

            public void All(List<SqlProjectReader.Scope> scopes, IReadOnlyDictionary<string, string> sources)
            {
                var owner = new Dictionary<string, SqlProjectReader.Scope>(StringComparer.Ordinal);
                foreach (var s in scopes)
                {
                    Candidate(ScopeModule(s), s.ModuleIdentity, -1);
                    foreach (var f in s.Files) owner[f] = s;
                }
                // First pass: parse every file and pick each module's first spelling before anything is emitted.
                var files = new List<(string Path, string Text, SqlSourceReader.Result Result,
                    List<(SqlSourceReader.Batch, TSqlStatement, (string, string, bool, List<(string, TSqlFragment)>))> Decls)>();
                foreach (var path in owner.Keys.OrderBy(p => p, StringComparer.Ordinal))
                {
                    if (!sources.TryGetValue(path, out var text)) continue;
                    var result = SqlSourceReader.Read(path, text);
                    var decls = new List<(SqlSourceReader.Batch, TSqlStatement, (string, string, bool, List<(string, TSqlFragment)>))>();
                    foreach (var pb in result.Batches)
                    {
                        if (pb.Fragment is not TSqlScript script) continue;
                        foreach (var stmt in script.Batches.SelectMany(b => b.Statements))
                        {
                            var decl = Declare(stmt, owner[path].DefaultSchema);
                            if (decl == null) continue;
                            decls.Add((pb.Batch, stmt, decl.Value));
                            Candidate(Declared(owner[path], decl.Value.Item1), path, pb.Batch.StartOffset + stmt.StartOffset);
                        }
                    }
                    files.Add((path, text, result, decls));
                }
                foreach (var m in modules) claimed[m.Key] = "module " + m.Value.Fqn;
                foreach (var s in scopes)
                {
                    Module(Canonical(ScopeModule(s)));
                    firsts[s] = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
                    Index[s] = new Dictionary<string, Declaration>(StringComparer.OrdinalIgnoreCase);
                }
                foreach (var f in files)
                {
                    foreach (var d in f.Result.Diagnostics) errors.WriteLine(d);
                    EmitFile(owner[f.Path], f.Path, f.Text, f.Decls);
                }
            }

            void EmitFile(SqlProjectReader.Scope scope, string rel, string text,
                List<(SqlSourceReader.Batch, TSqlStatement, (string, string, bool, List<(string, TSqlFragment)>))> decls)
            {
                string abs = root + "/" + rel, scopeModule = Canonical(ScopeModule(scope));
                var map = new Positions(text);
                Write(new { type = "file", path = abs, parent = scopeModule, start_line = 1, end_line = map.LineCount });
                foreach (var (batch, stmt, (schema, name, isFunction, columns)) in decls)
                {
                    string parent = Canonical(Declared(scope, schema));
                    string spelled = parent.Substring(parent.Length - schema.Length);
                    string identity = spelled + "." + name;
                    var (start, end, sl, el) = map.Span(batch, stmt);
                    if (firsts[scope].TryGetValue(identity, out var first))
                    {
                        Report(rel, batch, stmt, $"repeated declaration of {first}");
                        continue;
                    }
                    firsts[scope][identity] = $"{identity}, first declared at {rel}:{sl}";
                    string fqn = Rel(parent) + "." + name;
                    if (claimed.TryGetValue(fqn, out var by))
                    {
                        Report(rel, batch, stmt, $"{identity} renders sql.{fqn}, already taken by {by}");
                        continue;
                    }
                    claimed[fqn] = $"{identity} at {rel}:{sl}";
                    Module(parent);
                    if (scope.IsProject && written.Add("contains\n" + scopeModule + "\n" + parent))
                        Write(new { type = "contains", from = scopeModule, to = parent });
                    string id = prefix + (++next);
                    if (isFunction)
                        Write(new { type = "function", id, parent, name, @params = new string[0], file = abs, path = abs,
                            start, end, start_line = sl, end_line = el });
                    else
                        Write(new { type = "struct", id, parent, name, path = abs, start, end, start_line = sl, end_line = el });
                    foreach (var (column, fragment) in columns)
                    {
                        var (cs, ce, csl, cel) = map.Span(batch, fragment);
                        string cfqn = fqn + "." + column;
                        if (claimed.TryGetValue(cfqn, out var cby))
                        {
                            Report(rel, batch, fragment, $"{identity}.{column} renders sql.{cfqn}, already taken by {cby}");
                            continue;
                        }
                        claimed[cfqn] = $"{identity}.{column} at {rel}:{csl}";
                        string cid = prefix + (++next);
                        Write(new { type = "struct", id = cid, parent = parent + "." + name, name = column, path = abs,
                            start = cs, end = ce, start_line = csl, end_line = cel });
                        Write(new { type = "contains", from = id, to = cid });
                    }
                    Index[scope][identity] = new Declaration(spelled, name, id, isFunction);
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
