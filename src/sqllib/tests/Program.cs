using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using Apg.SqlFrontend;
using Microsoft.SqlServer.TransactSql.ScriptDom;

namespace SqlFrontend.Tests
{
    /// Framework-free gate tests for the SQL frontend, run with `dotnet run` and this
    /// project's path (src/sqllib/tests/SqlFrontend.Tests.csproj). Prints FAIL lines and
    /// exits non-zero on any failure. An optional argument selects one tier (unit, int or e2e;
    /// none runs all three): unit and int are pure in-memory, e2e reads the committed fixtures.
    public static class TestMain
    {
        static int _failures;
        static int _checks;

        public static int Main(string[] args)
        {
            var tiers = new Dictionary<string, List<(string, Action)>>
            {
                ["unit"] = UnitTests(),
                ["int"] = IntTests(),
                ["e2e"] = new List<(string, Action)>
                {
                    ("ProjectFixtureScopes", TestProjectFixtureScopes),
                    ("EmitterFixture", TestEmitterFixture),
                    ("EmitterTargetsList", TestEmitterTargetsList),
                    ("EmitterCli", TestEmitterCli),
                },
            };
            if (args.Length > 1 || (args.Length == 1 && !tiers.ContainsKey(args[0])))
            {
                Console.WriteLine($"usage: SqlFrontend.Tests [unit|int|e2e] (got: {string.Join(" ", args)})");
                return 2;
            }
            var tests = args.Length == 1 ? tiers[args[0]] : tiers.Values.SelectMany(t => t).ToList();
            foreach (var (name, test) in tests)
            {
                int before = _failures;
                try { test(); }
                catch (Exception ex) { Fail($"{name}: unhandled exception {ex}"); }
                Console.WriteLine($"{(_failures == before ? "ok  " : "FAIL")} {name}");
            }
            if (_failures == 0)
            {
                Console.WriteLine($"ALL TESTS PASSED ({tests.Count} tests, {_checks} checks)");
                return 0;
            }
            Console.WriteLine($"{_failures} CHECK(S) FAILED");
            return 1;
        }

        static List<(string, Action)> UnitTests() =>
            new List<(string, Action)>
            {
                ("NeutraliseSqlcmdLinesKeepsPositions", TestNeutraliseSqlcmdLinesKeepsPositions),
                ("NeutraliseVariablesSameLength", TestNeutraliseVariablesSameLength),
                ("NeutraliseCrlf", TestNeutraliseCrlf),
                ("GoSplitting", TestGoSplitting),
                ("SemicolonFreeProcedureParses", TestSemicolonFreeProcedureParses),
                ("MiddleBatchErrorRecovers", TestMiddleBatchErrorRecovers),
                ("MySqlDialectRejected", TestMySqlDialectRejected),
                ("PostgresSerialParses", TestPostgresSerialParses),
                ("FormatDiagnostic", TestFormatDiagnostic),
                ("ProjectStyle", TestProjectStyle),
                ("ProjectDefaultSchema", TestProjectDefaultSchema),
                ("LegacyMembership", TestLegacyMembership),
                ("SdkMembership", TestSdkMembership),
                ("DeployScriptsAreLoose", TestDeployScriptsAreLoose),
                ("SharedFileOrdinalFirstProject", TestSharedFileOrdinalFirstProject),
                ("LooseScopeIdentity", TestLooseScopeIdentity),
                ("ScopesPartitionInput", TestScopesPartitionInput),
            };

        static void Fail(string msg) { _failures++; Console.WriteLine("FAIL: " + msg); }

        static void Eq<T>(T expected, T actual, string what)
        {
            _checks++;
            if (!EqualityComparer<T>.Default.Equals(expected, actual))
                Fail($"{what}: expected <{expected}>, got <{actual}>");
        }

        static (int, int) LineCol(string text, int index)
        {
            int line = 1 + text.Take(index).Count(c => c == '\n');
            return (line, index - (text.LastIndexOf('\n', Math.Max(index - 1, 0)) + 1) + 1);
        }

        static int LineCount(string text) => text.Split('\n').Length;

        static TSqlStatement FirstStatement(SqlSourceReader.ParsedBatch b) =>
            ((TSqlScript)b.Fragment).Batches[0].Statements[0];

        static List<SqlSourceReader.ParsedBatch> NonEmpty(SqlSourceReader.Result r) =>
            r.Batches.Where(b => ((TSqlScript)b.Fragment).Batches.Count > 0).ToList();

        static void TestNeutraliseSqlcmdLinesKeepsPositions()
        {
            string text = ":setvar DbName Shop\n  :r .\\Tables\\Orders.sql\n:on error exit\nCREATE TABLE dbo.T (id INT)\n";
            string n = SqlSourceReader.Neutralise(text);
            Eq(text.Length, n.Length, "length");
            Eq(LineCount(text), LineCount(n), "line count");
            Eq(LineCol(text, text.IndexOf("CREATE")), LineCol(n, n.IndexOf("CREATE")), "CREATE line:col");
            Eq(false, n.Contains(':'), "command lines blanked");
            var r = SqlSourceReader.Read("a.sql", text);
            Eq(0, r.Diagnostics.Count, "diagnostics");
            var stmt = FirstStatement(NonEmpty(r)[0]);
            Eq((4, 1), (stmt.StartLine, stmt.StartColumn), "parsed CREATE line:col");
        }

        static void TestNeutraliseVariablesSameLength()
        {
            string text = "CREATE TABLE [x$(DbName)y] (id INT)\nSELECT 'a$(Env)b'\n";
            string n = SqlSourceReader.Neutralise(text);
            Eq(text.Length, n.Length, "length");
            Eq("CREATE TABLE [x__DbName_y] (id INT)\nSELECT 'a__Env_b'\n", n, "placeholders");
            Eq(0, SqlSourceReader.Read("a.sql", text).Diagnostics.Count, "diagnostics");
        }

        static void TestNeutraliseCrlf()
        {
            string text = ":setvar X 1\r\nCREATE TABLE t (id INT)\r\n";
            string n = SqlSourceReader.Neutralise(text);
            Eq(text.Length, n.Length, "length");
            Eq(new string(' ', 11) + "\r\nCREATE TABLE t (id INT)\r\n", n, "CRLF kept");
        }

        static void TestGoSplitting()
        {
            string text = string.Join("\n",
                "CREATE TABLE a (id INT)", // 1
                "go",                      // 2
                "CREATE TABLE b (id INT)", // 3
                "Go 2",                    // 4
                "SELECT 1",                // 5
                "GO -- done",              // 6
                "SELECT '",                // 7
                "GO",                      // 8 inside a string
                "'",                       // 9
                "/*",                      // 10
                "GO",                      // 11 inside a block comment
                "*/",                      // 12
                "SELECT 2");               // 13
            var batches = SqlSourceReader.SplitBatches(text);
            Eq("1,3,5,7", string.Join(",", batches.Select(b => b.StartLine)), "batch start lines");
            var r = SqlSourceReader.Read("a.sql", text);
            Eq(4, r.Batches.Count, "parsed batches");
            Eq(0, r.Diagnostics.Count, "diagnostics");

            // Whitespace-only batches around a leading/trailing GO are kept (empty fragments).
            var padded = SqlSourceReader.SplitBatches("GO\nSELECT 1\nGO\n");
            Eq("1,2,4", string.Join(",", padded.Select(b => b.StartLine)), "padded start lines");
        }

        static void TestSemicolonFreeProcedureParses()
        {
            var r = SqlSourceReader.Read("p.sql", "CREATE PROCEDURE dbo.p AS\nSELECT 1\nSELECT 2\n");
            Eq(0, r.Diagnostics.Count, "diagnostics");
            Eq(1, r.Batches.Count, "batches");
            Eq(true, FirstStatement(r.Batches[0]) is CreateProcedureStatement, "is CREATE PROCEDURE");
        }

        static void TestMiddleBatchErrorRecovers()
        {
            string text = string.Join("\n",
                "CREATE TABLE a (id INT)",  // 1
                "GO",                       // 2
                "SELECT 1",                 // 3
                "SELECT 2 FROM t WHERE )",  // 4 error at col 23
                "GO",                       // 5
                "CREATE TABLE b (id INT)"); // 6
            var r = SqlSourceReader.Read("m.sql", text);
            Eq("1,6", string.Join(",", r.Batches.Select(b => b.Batch.StartLine)), "parsed batch start lines");
            Eq(1, r.Diagnostics.Count, "diagnostics");
            if (r.Diagnostics.Count == 1)
                Eq(("m.sql", 4, 23), (r.Diagnostics[0].Path, r.Diagnostics[0].Line, r.Diagnostics[0].Column), "error position");
        }

        static void TestMySqlDialectRejected()
        {
            foreach (var mysql in new[]
            {
                "CREATE TABLE t (id INT AUTO_INCREMENT PRIMARY KEY)",
                "CREATE TABLE `t` (`id` INT)",
                "SELECT id FROM t LIMIT 10",
            })
            {
                var r = SqlSourceReader.Read("my.sql", "CREATE TABLE a (id INT)\nGO\n" + mysql + "\n");
                Eq(1, r.Diagnostics.Count, $"diagnostics for {mysql}");
                Eq(1, r.Batches.Count, $"T-SQL batch parsed beside {mysql}");
                if (r.Diagnostics.Count == 1) Eq(3, r.Diagnostics[0].Line, $"diagnostic line for {mysql}");
            }
        }

        static void TestPostgresSerialParses()
        {
            var r = SqlSourceReader.Read("pg.sql", "CREATE TABLE t (id SERIAL)\n");
            Eq(0, r.Diagnostics.Count, "diagnostics");
            Eq(1, r.Batches.Count, "batches");
        }

        static void TestFormatDiagnostic()
        {
            Eq("sql: db/a.sql:3:7: Incorrect syntax", SqlSourceReader.FormatDiagnostic("db/a.sql", 3, 7, "Incorrect syntax"), "format");
            Eq("sql: x.sql:1:2: m", new SqlSourceReader.Diagnostic("x.sql", 1, 2, "m").ToString(), "Diagnostic.ToString");
        }

        const string LegacyHead =
            "<Project DefaultTargets=\"Build\" ToolsVersion=\"4.0\" xmlns=\"http://schemas.microsoft.com/developer/msbuild/2003\">\n" +
            "  <Import Project=\"$(SQLDBExtensionsRefPath)\\Microsoft.Data.Tools.Schema.SqlTasks.targets\" />\n";

        static SqlProjectReader.Project Legacy(string path, string body) =>
            SqlProjectReader.ParseProject(path, LegacyHead + body + "</Project>");

        static SqlProjectReader.Project Sdk(string path, string body) =>
            SqlProjectReader.ParseProject(path, "<Project Sdk=\"Microsoft.Build.Sql/0.2.0-preview\">\n" + body + "</Project>");

        // The ModuleIdentity of every scope holding the file, joined (one entry when the file is scoped once).
        static string Owner(List<SqlProjectReader.Scope> scopes, string file) =>
            string.Join("|", scopes.Where(s => s.Files.Contains(file)).Select(s => s.ModuleIdentity));

        static void TestProjectStyle()
        {
            Eq(true, SqlProjectReader.ParseProject("a.sqlproj", "<Project Sdk=\"Microsoft.Build.Sql\" />").SdkStyle, "Sdk attribute");
            Eq(true, SqlProjectReader.ParseProject("a.sqlproj",
                "<Project><Sdk Name=\"Microsoft.Build.Sql\" Version=\"0.2.0-preview\" /></Project>").SdkStyle, "Sdk child element");
            Eq(true, SqlProjectReader.ParseProject("a.sqlproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />").SdkStyle, "non-SQL SDK name");
            Eq(false, Legacy("a.sqlproj", "").SdkStyle, "SSDT ToolsVersion + Import is legacy");
        }

        static void TestProjectDefaultSchema()
        {
            Eq("sales", Sdk("a.sqlproj", "<PropertyGroup><DefaultSchema>sales</DefaultSchema></PropertyGroup>").DefaultSchema, "taken");
            Eq("dbo", Legacy("a.sqlproj", "<PropertyGroup><Name>Db</Name></PropertyGroup>").DefaultSchema, "absent");
            Eq("dbo", Legacy("a.sqlproj",
                "<PropertyGroup Condition=\" '$(Configuration)' == 'Debug' \"><DefaultSchema>dev</DefaultSchema></PropertyGroup>").DefaultSchema,
                "Condition-guarded PropertyGroup ignored");
            Eq("dbo", Legacy("a.sqlproj",
                "<PropertyGroup><DefaultSchema Condition=\"'$(X)' == '1'\">dev</DefaultSchema></PropertyGroup>").DefaultSchema,
                "Condition-guarded property ignored");
        }

        static void TestLegacyMembership()
        {
            var lit = Legacy("lit/Lit.sqlproj", "<ItemGroup><Build Include=\"Tables\\Orders.sql\" /></ItemGroup>");
            var star = Legacy("star/Star.sqlproj",
                "<ItemGroup><Build Include=\"Tables\\*.sql\" /><Build Remove=\"Tables\\Old.sql\" /></ItemGroup>");
            var deep = Legacy("deep/Deep.sqlproj", "<ItemGroup><Build Include=\"**\\*.sql\" /></ItemGroup>");
            var cond = Legacy("cond/Cond.sqlproj",
                "<ItemGroup Condition=\"'$(X)' == '1'\"><Build Include=\"a.sql\" /></ItemGroup>");
            var scopes = SqlProjectReader.BuildScopes(new[] { lit, star, deep, cond }, new[]
            {
                "lit/Tables/Orders.sql", "lit/Tables/Unnamed.sql",
                "star/Tables/A.sql", "star/Tables/Old.sql", "star/Tables/Sub/B.sql",
                "deep/x.sql", "deep/Tables/Sub/y.sql",
                "cond/a.sql",
            });
            Eq("lit/Lit.sqlproj", Owner(scopes, "lit/Tables/Orders.sql"), "literal backslash include");
            Eq("lit/Tables", Owner(scopes, "lit/Tables/Unnamed.sql"), "unnamed file is loose");
            Eq("star/Star.sqlproj", Owner(scopes, "star/Tables/A.sql"), "Tables\\*.sql");
            Eq("star/Tables", Owner(scopes, "star/Tables/Old.sql"), "Build Remove subtracts");
            Eq("star/Tables/Sub", Owner(scopes, "star/Tables/Sub/B.sql"), "* does not cross directories");
            Eq("deep/Deep.sqlproj", Owner(scopes, "deep/x.sql"), "**\\*.sql top level");
            Eq("deep/Deep.sqlproj", Owner(scopes, "deep/Tables/Sub/y.sql"), "**\\*.sql nested");
            Eq("cond", Owner(scopes, "cond/a.sql"), "Condition-guarded ItemGroup adds nothing");
        }

        static void TestSdkMembership()
        {
            var p = Sdk("sdk/Db.sqlproj",
                "<ItemGroup>\n" +
                "  <Build Include=\"..\\Shared\\s.sql\" />\n" +
                "  <Build Remove=\"Old\\r.sql\" />\n" +
                "  <None Include=\"Notes\\n.sql\" />\n" +
                "  <PreDeploy Include=\"Scripts\\Pre.sql\" />\n" +
                "  <PostDeploy Include=\"Scripts\\Post.sql\" />\n" +
                "</ItemGroup>\n");
            var scopes = SqlProjectReader.BuildScopes(new[] { p }, new[]
            {
                "sdk/a.sql", "sdk/x/b.sql", "sdk/bin/c.sql", "sdk/obj/Debug/d.sql", "Shared/s.sql", "Shared/t.sql",
                "sdk/Old/r.sql", "sdk/Notes/n.sql", "sdk/Scripts/Pre.sql", "sdk/Scripts/Post.sql", "other/o.sql",
            });
            Eq("sdk/Db.sqlproj", Owner(scopes, "sdk/a.sql"), "top-level file");
            Eq("sdk/Db.sqlproj", Owner(scopes, "sdk/x/b.sql"), "nested file");
            Eq("sdk/bin", Owner(scopes, "sdk/bin/c.sql"), "bin/ excluded");
            Eq("sdk/obj/Debug", Owner(scopes, "sdk/obj/Debug/d.sql"), "obj/ excluded");
            Eq("sdk/Db.sqlproj", Owner(scopes, "Shared/s.sql"), "..\\Shared Build Include added");
            Eq("Shared", Owner(scopes, "Shared/t.sql"), "unnamed outside file stays loose");
            Eq("sdk/Old", Owner(scopes, "sdk/Old/r.sql"), "Build Remove subtracted");
            Eq("sdk/Notes", Owner(scopes, "sdk/Notes/n.sql"), "None subtracted");
            Eq("sdk/Scripts", Owner(scopes, "sdk/Scripts/Pre.sql"), "PreDeploy subtracted");
            Eq("sdk/Scripts", Owner(scopes, "sdk/Scripts/Post.sql"), "PostDeploy subtracted");
            Eq("other", Owner(scopes, "other/o.sql"), "outside the project directory");
        }

        static void TestDeployScriptsAreLoose()
        {
            // The SSDT way: a deploy script is a PreDeploy/PostDeploy item, not also a Build item.
            var legacy = Legacy("leg/Leg.sqlproj",
                "<ItemGroup><Build Include=\"T.sql\" /><PreDeploy Include=\"Scripts\\Pre.sql\" />" +
                "<PostDeploy Include=\"Scripts\\Post.sql\" /></ItemGroup>");
            var sdk = Sdk("sdk/Sdk.sqlproj",
                "<ItemGroup><PreDeploy Include=\"Scripts\\Pre.sql\" /><PostDeploy Include=\"Scripts\\Post.sql\" /></ItemGroup>");
            Eq("Scripts/Pre.sql", string.Join(",", legacy.PreDeploys), "legacy PreDeploy parsed");
            Eq("Scripts/Post.sql", string.Join(",", sdk.PostDeploys), "SDK PostDeploy parsed");
            var scopes = SqlProjectReader.BuildScopes(new[] { legacy, sdk }, new[]
            {
                "leg/T.sql", "leg/Scripts/Pre.sql", "leg/Scripts/Post.sql",
                "sdk/T.sql", "sdk/Scripts/Pre.sql", "sdk/Scripts/Post.sql",
            });
            Eq("leg/Leg.sqlproj", Owner(scopes, "leg/T.sql"), "legacy build member");
            Eq("leg/Scripts", Owner(scopes, "leg/Scripts/Pre.sql"), "legacy PreDeploy loose");
            Eq("leg/Scripts", Owner(scopes, "leg/Scripts/Post.sql"), "legacy PostDeploy loose");
            Eq("sdk/Sdk.sqlproj", Owner(scopes, "sdk/T.sql"), "SDK build member");
            Eq("sdk/Scripts", Owner(scopes, "sdk/Scripts/Pre.sql"), "SDK PreDeploy loose");
            Eq("sdk/Scripts", Owner(scopes, "sdk/Scripts/Post.sql"), "SDK PostDeploy loose");
        }

        static void TestSharedFileOrdinalFirstProject()
        {
            var a = Legacy("a/A.sqlproj", "<ItemGroup><Build Include=\"..\\Shared\\x.sql\" /></ItemGroup>");
            var b = Legacy("b/B.sqlproj", "<ItemGroup><Build Include=\"..\\Shared\\x.sql\" /></ItemGroup>");
            var files = new[] { "Shared/x.sql" };
            Eq("a/A.sqlproj", Owner(SqlProjectReader.BuildScopes(new[] { b, a }, files), "Shared/x.sql"), "input order B,A");
            Eq("a/A.sqlproj", Owner(SqlProjectReader.BuildScopes(new[] { a, b }, files), "Shared/x.sql"), "input order A,B");
        }

        static void TestLooseScopeIdentity()
        {
            var scopes = SqlProjectReader.BuildScopes(new SqlProjectReader.Project[0],
                new[] { "db/x/y.sql", "root.sql", "db/x/U.SQL" });
            var nested = scopes.Single(s => s.ModuleIdentity == "db/x");
            Eq(("dbo", false), (nested.DefaultSchema, nested.IsProject), "nested loose scope schema/kind");
            Eq("db/x/U.SQL,db/x/y.sql", string.Join(",", nested.Files), "upper-case .SQL scoped");
            var root = scopes.Single(s => s.ModuleIdentity == "");
            Eq(("dbo", false, "root.sql"), (root.DefaultSchema, root.IsProject, string.Join(",", root.Files)), "scan-root loose scope");
        }

        static void TestScopesPartitionInput()
        {
            var p = Sdk("db/Db.sqlproj", "<PropertyGroup><DefaultSchema>app</DefaultSchema></PropertyGroup>");
            var q = Legacy("old/Old.sqlproj", "<ItemGroup><Build Include=\"t.sql\" /></ItemGroup>");
            var sql = new[] { "db/a.sql", "db/B.SQL", "db/bin/c.sql", "old/t.sql", "old/u.sql", "z.sql" };
            var input = sql.Concat(new[] { "db/Db.sqlproj", "old/Old.sqlproj" }).ToList();
            var scopes = SqlProjectReader.BuildScopes(new[] { q, p }, input);
            foreach (var f in sql)
                Eq(1, scopes.Count(s => s.Files.Contains(f)), $"{f} in exactly one scope");
            Eq(sql.Length, scopes.Sum(s => s.Files.Count), "no extra scoped files");
            Eq(false, scopes.Any(s => s.Files.Any(f => f.EndsWith(".sqlproj"))), "no .sqlproj scoped");
            var proj = scopes.Single(s => s.IsProject && s.Files.Contains("db/a.sql"));
            Eq(("db/Db.sqlproj", "app"), (proj.ModuleIdentity, proj.DefaultSchema), "project scope identity/schema");
            Eq("db/Db.sqlproj", Owner(scopes, "db/B.SQL"), "upper-case .SQL project member");
        }

        // int: SchemaEmitter.Emit wired to SqlSourceReader and SqlProjectReader over inline texts; /repo is never touched.
        static List<(string, Action)> IntTests() =>
            new List<(string, Action)>
            {
                ("EmitDeclarations", TestEmitDeclarations),
                ("EmitLooseRepeatKeepsFirstSpelling", TestEmitLooseRepeatKeepsFirstSpelling),
                ("EmitSchemaCaseFollowsFirstDeclaration", TestEmitSchemaCaseFollowsFirstDeclaration),
                ("EmitLooseDirectoriesMergeByCase", TestEmitLooseDirectoriesMergeByCase),
                ("EmitModuleCollisionDropsTable", TestEmitModuleCollisionDropsTable),
                ("EmitViewDuplicateColumnKeepsOne", TestEmitViewDuplicateColumnKeepsOne),
            };

        const string Root = "/repo";

        static (List<JsonElement>, List<string>) RunEmit(IEnumerable<SqlProjectReader.Project> projects,
            Dictionary<string, string> sources)
        {
            var scopes = SqlProjectReader.BuildScopes(projects, sources.Keys);
            var stdout = new StringWriter();
            var stderr = new StringWriter();
            SchemaEmitter.Emit(Root, scopes, sources, "n", stdout, stderr);
            var recs = new List<JsonElement>();
            foreach (var line in stdout.ToString().Split('\n', StringSplitOptions.RemoveEmptyEntries))
            {
                using var doc = JsonDocument.Parse(line);
                recs.Add(doc.RootElement.Clone());
            }
            return (recs, stderr.ToString().Split('\n', StringSplitOptions.RemoveEmptyEntries).ToList());
        }

        static string S(JsonElement r, string prop) =>
            r.TryGetProperty(prop, out var v) ? (v.ValueKind == JsonValueKind.String ? v.GetString() : v.ToString()) : null;

        static int I(JsonElement r, string prop) => r.GetProperty(prop).GetInt32();

        static List<JsonElement> Of(List<JsonElement> recs, string type) => recs.Where(r => S(r, "type") == type).ToList();

        static JsonElement One(List<JsonElement> recs, string type, string parent, string name)
        {
            var m = recs.Where(r => S(r, "type") == type && S(r, "parent") == parent && S(r, "name") == name).ToList();
            Eq(1, m.Count, $"{type} {parent}.{name} emitted once");
            return m.FirstOrDefault();
        }

        static JsonElement FileRec(List<JsonElement> recs, string path) => Of(recs, "file").Single(r => S(r, "path") == path);

        static string Modules(List<JsonElement> recs) =>
            string.Join(",", Of(recs, "module").Select(r => S(r, "fqn")).OrderBy(f => f, StringComparer.Ordinal));

        // Names of the struct records the contains edges from `id` reach, in emission order.
        static string ColumnsOf(List<JsonElement> recs, JsonElement owner)
        {
            string id = S(owner, "id");
            var kids = new HashSet<string>(Of(recs, "contains").Where(c => S(c, "from") == id).Select(c => S(c, "to")));
            return string.Join(",", Of(recs, "struct").Where(r => kids.Contains(S(r, "id"))).Select(r => S(r, "name")));
        }

        static JsonElement Column(List<JsonElement> recs, JsonElement owner, string name) =>
            One(recs, "struct", S(owner, "parent") + "." + S(owner, "name"), name);

        // Every module identity non-empty and unprefixed; every contains edge after the records of both endpoints.
        static void StreamShape(List<JsonElement> recs)
        {
            var seen = new HashSet<string>(StringComparer.Ordinal);
            foreach (var r in recs)
            {
                string type = S(r, "type");
                if (type == "module")
                {
                    string fqn = S(r, "fqn");
                    Eq(false, string.IsNullOrEmpty(fqn) || fqn.StartsWith("sql.", StringComparison.Ordinal), $"module <{fqn}> identity");
                    seen.Add(fqn);
                }
                else if (type == "contains")
                    Eq(true, seen.Contains(S(r, "from")) && seen.Contains(S(r, "to")),
                        $"contains {S(r, "from")}->{S(r, "to")} follows its endpoints");
                else if (S(r, "id") != null) seen.Add(S(r, "id"));
            }
        }

        static int Utf8(string text, int index) => Encoding.UTF8.GetByteCount(text.Substring(0, index));

        static void TestEmitDeclarations()
        {
            const string P = Root + "/db/Db.sqlproj";
            string app = P + ".app", sales = P + ".sales";
            var proj = Sdk("db/Db.sqlproj", "<PropertyGroup><DefaultSchema>app</DefaultSchema></PropertyGroup>\n");
            string tables = string.Join("\n",
                "-- café €",                                     // 1 multi-byte characters before every object
                "CREATE TABLE Orders (",                         // 2
                "  id INT,",                                     // 3
                "  name NVARCHAR(10)",                           // 4
                ")",                                             // 5
                "GO",                                            // 6
                "CREATE TABLE sales.Orders (id INT)",            // 7
                "CREATE TABLE #temp (id INT)",                   // 8
                "ALTER TABLE sales.Orders ADD qty INT",          // 9
                "GO",                                            // 10
                "CREATE TYPE Phone FROM NVARCHAR(20)",           // 11
                "CREATE TYPE Lines AS TABLE (sku INT, qty INT)", // 12
                "CREATE SEQUENCE Seq START WITH 1",              // 13
                "CREATE SYNONYM Syn FOR Orders");                // 14, no trailing newline
            string code = string.Join("\n",
                "CREATE PROCEDURE GetOrders AS SELECT 1",                                      // 1
                "GO",                                                                          // 2
                "CREATE FUNCTION sales.Total() RETURNS INT AS BEGIN RETURN 1 END",             // 3
                "GO",                                                                          // 4
                "CREATE FUNCTION Itvf() RETURNS TABLE AS RETURN SELECT 1 AS x",                // 5
                "GO",                                                                          // 6
                "CREATE FUNCTION Mtvf() RETURNS @r TABLE (x INT) AS BEGIN RETURN END",         // 7
                "GO",                                                                          // 8
                "CREATE TRIGGER TrgS ON sales.Orders AFTER INSERT AS SELECT 1",                // 9
                "GO",                                                                          // 10
                "CREATE TRIGGER TrgD ON DATABASE FOR CREATE_TABLE AS SELECT 1",                // 11
                "GO",                                                                          // 12
                "CREATE OR ALTER PROCEDURE Upsert AS SELECT 1",                                // 13
                "GO",                                                                          // 14
                "ALTER VIEW AltV AS SELECT 1 AS a",                                            // 15
                "GO",                                                                          // 16
                "ALTER FUNCTION AltF() RETURNS INT AS BEGIN RETURN 1 END",                     // 17
                "GO",                                                                          // 18
                "CREATE OR ALTER TRIGGER TrgO ON Orders AFTER UPDATE AS SELECT 1",             // 19
                "GO",                                                                          // 20
                "CREATE VIEW Vsel AS SELECT o.id, o.name AS label, o.id + 1, * FROM Orders o", // 21
                "GO",                                                                          // 22
                "CREATE VIEW Vcols (a,",                                                       // 23
                "  b) AS SELECT 1, 2",                                                         // 24
                "GO",                                                                          // 25
                "CREATE PROCEDURE Multi AS",                                                   // 26
                "SELECT 1",                                                                    // 27
                "SELECT 2",                                                                    // 28
                "GO",                                                                          // 29
                "ALTER PROCEDURE GetOrders AS SELECT 2") + "\n";                               // 30
            var (recs, err) = RunEmit(new[] { proj }, new Dictionary<string, string>
            {
                ["db/Tables.sql"] = tables,
                ["db/Code.sql"] = code,
                ["scripts/load.sql"] = "CREATE TABLE Staging (id INT)\n",
                ["root.sql"] = "CREATE TABLE RootT (id INT)\nCREATE TABLE sales.Two (id INT)",
            });
            StreamShape(recs);

            // Modules: project, its schemas, loose directory, scan root, bare loose schemas.
            Eq(string.Join(",", new[] { Root, P, app, sales, Root + "/scripts", "dbo", "sales" }.OrderBy(f => f, StringComparer.Ordinal)),
                Modules(recs), "module records");
            foreach (var schema in new[] { app, sales })
                Eq(1, Of(recs, "contains").Count(c => S(c, "from") == P && S(c, "to") == schema), $"contains {P}->{schema}");

            // Files: member -> P, loose -> its directory module, scan-root file -> /repo; end_line = line count.
            foreach (var (path, parent, lines) in new[]
            {
                ("db/Tables.sql", P, 14), ("db/Code.sql", P, 30), ("scripts/load.sql", Root + "/scripts", 1), ("root.sql", Root, 2),
            })
            {
                var f = FileRec(recs, Root + "/" + path);
                Eq((parent, 1, lines), (S(f, "parent"), I(f, "start_line"), I(f, "end_line")), $"file {path}");
            }

            // Structs: table, temp table, alias type, table type, sequence, synonym, views; one-part names take app.
            var orders = One(recs, "struct", app, "Orders");
            var salesOrders = One(recs, "struct", sales, "Orders");
            var temp = One(recs, "struct", app, "#temp");
            One(recs, "struct", app, "Phone");
            var lines12 = One(recs, "struct", app, "Lines");
            One(recs, "struct", app, "Seq");
            One(recs, "struct", app, "Syn");
            One(recs, "struct", app, "AltV");
            var vsel = One(recs, "struct", app, "Vsel");
            var vcols = One(recs, "struct", app, "Vcols");
            One(recs, "struct", "dbo", "Staging");
            One(recs, "struct", "dbo", "RootT");
            One(recs, "struct", "sales", "Two");

            // Functions: procedures, scalar/inline/multi-statement functions, DML and DDL triggers, CREATE OR ALTER and ALTER.
            foreach (var (parent, name) in new[]
            {
                (app, "GetOrders"), (sales, "Total"), (app, "Itvf"), (app, "Mtvf"), (sales, "TrgS"), (app, "TrgD"),
                (app, "Upsert"), (app, "AltF"), (app, "TrgO"), (app, "Multi"),
            })
                One(recs, "function", parent, name);
            Eq(10, Of(recs, "function").Count, "function records");

            // Columns contained by their object; ALTER TABLE adds none; view select list names alias/column refs only.
            Eq("id,name", ColumnsOf(recs, orders), "Orders columns");
            Eq("id", ColumnsOf(recs, salesOrders), "sales.Orders columns (ALTER TABLE declares nothing)");
            Eq("id", ColumnsOf(recs, temp), "#temp columns");
            Eq("sku,qty", ColumnsOf(recs, lines12), "table type columns");
            Eq("id,label", ColumnsOf(recs, vsel), "view select-list columns");
            Eq("a,b", ColumnsOf(recs, vcols), "view column-list columns");

            // Spans: original-file lines across GO batches; start/end are UTF-8 bytes after the multi-byte comment.
            int ordersAt = tables.IndexOf("CREATE TABLE Orders"), ordersEnd = tables.IndexOf(")\nGO") + 1;
            Eq((2, 5, Utf8(tables, ordersAt), Utf8(tables, ordersEnd)),
                (I(orders, "start_line"), I(orders, "end_line"), I(orders, "start"), I(orders, "end")), "Orders span");
            int idAt = tables.IndexOf("id INT,");
            var id = Column(recs, orders, "id");
            Eq((3, 3, Utf8(tables, idAt), Utf8(tables, idAt + "id INT".Length)),
                (I(id, "start_line"), I(id, "end_line"), I(id, "start"), I(id, "end")), "Orders.id span");
            Eq((4, 4), (I(Column(recs, orders, "name"), "start_line"), I(Column(recs, orders, "name"), "end_line")), "Orders.name lines");
            Eq((7, 7, Utf8(tables, tables.IndexOf("CREATE TABLE sales"))),
                (I(salesOrders, "start_line"), I(salesOrders, "end_line"), I(salesOrders, "start")), "sales.Orders span after GO");
            Eq((12, 12), (I(lines12, "start_line"), I(lines12, "end_line")), "Lines span");
            Eq((21, 21), (I(Column(recs, vsel, "label"), "start_line"), I(Column(recs, vsel, "label"), "end_line")), "Vsel.label lines");
            Eq((23, 24), (I(vcols, "start_line"), I(vcols, "end_line")), "Vcols span");
            Eq(23, I(Column(recs, vcols, "a"), "start_line"), "Vcols.a line");
            Eq(24, I(Column(recs, vcols, "b"), "start_line"), "Vcols.b line");
            var multi = One(recs, "function", app, "Multi");
            Eq((26, 28), (I(multi, "start_line"), I(multi, "end_line")), "Multi span");
            Eq((3, 3), (I(One(recs, "function", sales, "Total"), "start_line"), I(One(recs, "function", sales, "Total"), "end_line")), "Total span");
            Eq(1, I(One(recs, "function", app, "GetOrders"), "start_line"), "GetOrders keeps its first declaration");

            // A later ALTER of an already-declared procedure in the scope is a repeat: one diagnostic, nothing emitted.
            Eq("sql: db/Code.sql:30:1: repeated declaration of app.GetOrders, first declared at db/Code.sql:1",
                string.Join("\n", err), "diagnostics");
        }

        static void TestEmitLooseRepeatKeepsFirstSpelling()
        {
            var (recs, err) = RunEmit(new SqlProjectReader.Project[0], new Dictionary<string, string>
            {
                ["db/a.sql"] = "CREATE TABLE [Dbo].[Orders] (id INT)\n",
                ["db/b.sql"] = "\nCREATE TABLE DBO.ORDERS (id INT)\n",
            });
            StreamShape(recs);
            Eq("/repo/db,Dbo", Modules(recs), "module records");
            One(recs, "struct", "Dbo", "Orders");
            Eq(2, Of(recs, "struct").Count, "one table plus its column");
            Eq("sql: db/b.sql:2:1: repeated declaration of Dbo.Orders, first declared at db/a.sql:1",
                string.Join("\n", err), "diagnostics");
        }

        static void TestEmitSchemaCaseFollowsFirstDeclaration()
        {
            const string P = Root + "/db/Db.sqlproj";
            var (recs, err) = RunEmit(new[] { Sdk("db/Db.sqlproj", "") }, new Dictionary<string, string>
            {
                ["db/a.sql"] = "CREATE TABLE Sales.A (id INT)\n",
                ["db/b.sql"] = "CREATE TABLE SALES.B (id INT)\n",
            });
            StreamShape(recs);
            Eq(string.Join(",", new[] { P, P + ".Sales" }), Modules(recs), "module records");
            One(recs, "struct", P + ".Sales", "A");
            One(recs, "struct", P + ".Sales", "B");
            Eq(0, err.Count, "diagnostics");
        }

        static void TestEmitLooseDirectoriesMergeByCase()
        {
            var (recs, err) = RunEmit(new SqlProjectReader.Project[0], new Dictionary<string, string>
            {
                ["Scripts/a.sql"] = "CREATE TABLE dbo.A (id INT)\n",
                ["scripts/b.sql"] = "CREATE TABLE dbo.B (id INT)\n",
            });
            StreamShape(recs);
            Eq("/repo/Scripts,dbo", Modules(recs), "one directory module spelled /repo/Scripts");
            Eq("/repo/Scripts", S(FileRec(recs, Root + "/Scripts/a.sql"), "parent"), "Scripts/a.sql parent");
            Eq("/repo/Scripts", S(FileRec(recs, Root + "/scripts/b.sql"), "parent"), "scripts/b.sql parent");
            One(recs, "struct", "dbo", "A");
            One(recs, "struct", "dbo", "B");
            Eq(0, err.Count, "diagnostics");
        }

        static void TestEmitModuleCollisionDropsTable()
        {
            var (recs, err) = RunEmit(new SqlProjectReader.Project[0], new Dictionary<string, string>
            {
                ["dbo.Orders/x.sql"] = "CREATE TABLE Other (id INT)\n",
                ["root.sql"] = "CREATE TABLE dbo.Orders (id INT, qty INT)\n",
            });
            StreamShape(recs);
            Eq("/repo,/repo/dbo.Orders,dbo", Modules(recs), "module records");
            Eq(0, recs.Count(r => S(r, "name") == "Orders" || S(r, "parent") == "dbo.Orders"), "table and its columns dropped");
            Eq("/repo", S(FileRec(recs, Root + "/root.sql"), "parent"), "root.sql still emitted");
            One(recs, "struct", "dbo", "Other");
            Eq("sql: root.sql:1:1: dbo.Orders renders sql.dbo.Orders, already taken by module /repo/dbo.Orders",
                string.Join("\n", err), "diagnostics");
        }

        static void TestEmitViewDuplicateColumnKeepsOne()
        {
            var (recs, _) = RunEmit(new SqlProjectReader.Project[0], new Dictionary<string, string>
            {
                ["v.sql"] = "CREATE VIEW dbo.V AS SELECT o.id, p.id FROM dbo.T o JOIN dbo.T p ON 1 = 1\n",
            });
            StreamShape(recs);
            Eq("id", ColumnsOf(recs, One(recs, "struct", "dbo", "V")), "one id column");
        }

        // e2e: the committed tests/fixtures/projects tree, located from the test binary.
        static string FixtureRoot(string name)
        {
            for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir != null; dir = dir.Parent)
                if (File.Exists(Path.Combine(dir.FullName, "SqlFrontend.Tests.csproj")))
                    return Path.Combine(dir.FullName, "fixtures", name);
            throw new InvalidOperationException("SqlFrontend.Tests.csproj not found above " + AppContext.BaseDirectory);
        }

        static void TestProjectFixtureScopes()
        {
            string root = FixtureRoot("projects");
            var all = Directory.EnumerateFiles(root, "*", SearchOption.AllDirectories)
                .Select(p => Path.GetRelativePath(root, p).Replace('\\', '/')).ToList();
            var projs = all.Where(p => p.EndsWith(".sqlproj", StringComparison.OrdinalIgnoreCase)).ToList();
            var sql = all.Where(p => p.EndsWith(".sql", StringComparison.OrdinalIgnoreCase)).ToList();
            var scopes = SqlProjectReader.Read(root, projs, sql);
            // file -> (module identity, default schema, is project)
            var expected = new Dictionary<string, (string, string, bool)>
            {
                ["Legacy/Tables/Orders.sql"] = ("Legacy/Legacy.sqlproj", "app", true),
                ["Shared/Common.sql"] = ("Legacy/Legacy.sqlproj", "app", true),
                ["Legacy/Tables/Old.sql"] = ("Legacy/Tables", "dbo", false),
                ["Legacy/Views/V.sql"] = ("Legacy/Views", "dbo", false),
                ["Legacy/Scripts/Pre.sql"] = ("Legacy/Scripts", "dbo", false),
                ["Legacy/Scripts/Post.sql"] = ("Legacy/Scripts", "dbo", false),
                ["Sdk/Tables/Customers.sql"] = ("Sdk/Sdk.sqlproj", "dbo", true),
                ["Shared/Extra.sql"] = ("Sdk/Sdk.sqlproj", "dbo", true),
                ["Sdk/Notes/Readme.sql"] = ("Sdk/Notes", "dbo", false),
                ["Sdk/bin/Debug/Sdk.publish.sql"] = ("Sdk/bin/Debug", "dbo", false),
                ["Sdk/obj/Debug/Generated.sql"] = ("Sdk/obj/Debug", "dbo", false),
                ["Shared/Unlisted.sql"] = ("Shared", "dbo", false),
                ["Other/Procs/GetAll.sql"] = ("Other/Other.sqlproj", "dbo", true),
                ["scripts/seed.sql"] = ("scripts", "dbo", false),
            };
            Eq("Legacy/Legacy.sqlproj,Other/Other.sqlproj,Sdk/Sdk.sqlproj",
                string.Join(",", projs.OrderBy(p => p, StringComparer.Ordinal)), "fixture projects");
            Eq(string.Join(",", expected.Keys.OrderBy(p => p, StringComparer.Ordinal)),
                string.Join(",", sql.OrderBy(p => p, StringComparer.Ordinal)), "fixture .sql files");
            foreach (var (file, want) in expected)
            {
                var owners = scopes.Where(s => s.Files.Contains(file)).ToList();
                Eq(1, owners.Count, $"{file} scoped once");
                if (owners.Count == 1)
                    Eq(want, (owners[0].ModuleIdentity, owners[0].DefaultSchema, owners[0].IsProject), $"{file} scope");
            }
            Eq(expected.Count, scopes.Sum(s => s.Files.Count), "no extra scoped files");
        }

        // e2e: the committed tests/fixtures/emitter tree, read through SchemaEmitter.Run and Program.Main.
        static string EmitterRoot() => Path.TrimEndingDirectorySeparator(Path.GetFullPath(FixtureRoot("emitter")));

        // Absolute path of every committed .sql and .sqlproj fixture under root, ordinal order.
        static List<string> FixtureTargets(string root) =>
            Directory.EnumerateFiles(root, "*", SearchOption.AllDirectories)
                .Where(p => p.EndsWith(".sql", StringComparison.OrdinalIgnoreCase) || p.EndsWith(".sqlproj", StringComparison.OrdinalIgnoreCase))
                .OrderBy(p => p, StringComparer.Ordinal).ToList();

        static (string, string) RunTargets(string root, IEnumerable<string> targets, IReadOnlyList<string> excludes, string prefix)
        {
            var stdout = new StringWriter();
            var stderr = new StringWriter();
            SchemaEmitter.Run(root, targets, new List<string>(), excludes, prefix, stdout, stderr);
            return (stdout.ToString(), stderr.ToString());
        }

        static List<JsonElement> ParseLines(string text)
        {
            var recs = new List<JsonElement>();
            foreach (var line in text.Split('\n', StringSplitOptions.RemoveEmptyEntries))
            {
                using var doc = JsonDocument.Parse(line);
                recs.Add(doc.RootElement.Clone());
            }
            return recs;
        }

        static void TestEmitterFixture()
        {
            string root = EmitterRoot();
            var targets = FixtureTargets(root);
            Eq(9, targets.Count, "emitter fixture .sql/.sqlproj files");
            var (output, errors) = RunTargets(root, targets, new List<string>(), "n");
            var recs = ParseLines(output);
            StreamShape(recs);
            string lp = root + "/Legacy/Legacy.sqlproj", sp = root + "/Sdk/Sdk.sqlproj";
            string lapp = lp + ".app", lsales = lp + ".sales", sdbo = sp + ".dbo";

            // Modules: absolute project and loose-directory identities, P.schema project schemas, bare loose dbo.
            Eq(string.Join(",", new[] { lp, lapp, lsales, sp, sdbo, root + "/Sdk/bin/Debug", root + "/Sdk/obj/Debug", root + "/scripts", "dbo" }
                .OrderBy(f => f, StringComparer.Ordinal)), Modules(recs), "module records");
            foreach (var (from, to) in new[] { (lp, lapp), (lp, lsales), (sp, sdbo) })
                Eq(1, Of(recs, "contains").Count(c => S(c, "from") == from && S(c, "to") == to), $"contains {from}->{to}");

            // Files: project members under P; the listed obj/ and bin/ files loose under their directory modules.
            foreach (var (rel, parent, lines) in new[]
            {
                ("Legacy/Schema/Code.sql", lp, 10), ("Legacy/Schema/Tables.sql", lp, 9), ("Sdk/Tables/Customers.sql", sp, 1),
                ("Sdk/bin/Debug/Published.sql", root + "/Sdk/bin/Debug", 1), ("Sdk/obj/Debug/Generated.sql", root + "/Sdk/obj/Debug", 1),
                ("scripts/a.sql", root + "/scripts", 1), ("scripts/b.sql", root + "/scripts", 2),
            })
            {
                var f = Of(recs, "file").Where(r => S(r, "path") == root + "/" + rel).ToList();
                Eq(1, f.Count, $"file {rel} emitted once");
                if (f.Count == 1) Eq((parent, 1, lines), (S(f[0], "parent"), I(f[0], "start_line"), I(f[0], "end_line")), $"file {rel}");
            }
            Eq(7, Of(recs, "file").Count, "file records");

            // Objects with parent, spelling, file and line span; columns as name:start-end in source order.
            foreach (var (type, parent, name, rel, sl, el, cols) in new[]
            {
                ("struct", lapp, "Orders", "Legacy/Schema/Tables.sql", 2, 5, "id:3-3,total:4-4"),
                ("struct", lsales, "Audit", "Legacy/Schema/Tables.sql", 7, 7, "id:7-7,note:7-7"),
                ("struct", lapp, "Lines", "Legacy/Schema/Tables.sql", 9, 9, "sku:9-9,qty:9-9"),
                ("struct", lapp, "OrderTotals", "Legacy/Schema/Code.sql", 1, 3, "id:2-2,amount:2-2"),
                ("function", lapp, "GetOrders", "Legacy/Schema/Code.sql", 5, 7, ""),
                ("function", lsales, "OrdersAudit", "Legacy/Schema/Code.sql", 9, 10, ""),
                ("struct", sdbo, "Customers", "Sdk/Tables/Customers.sql", 1, 1, "id:1-1"),
                ("struct", "dbo", "Published", "Sdk/bin/Debug/Published.sql", 1, 1, "id:1-1"),
                ("struct", "dbo", "Generated", "Sdk/obj/Debug/Generated.sql", 1, 1, "id:1-1"),
                ("struct", "dbo", "Items", "scripts/a.sql", 1, 1, "id:1-1"),
            })
            {
                var o = One(recs, type, parent, name);
                if (o.ValueKind == JsonValueKind.Undefined) continue;
                Eq((root + "/" + rel, sl, el), (S(o, "path"), I(o, "start_line"), I(o, "end_line")), $"{type} {parent}.{name}");
                string id = S(o, "id");
                var kids = new HashSet<string>(Of(recs, "contains").Where(c => S(c, "from") == id).Select(c => S(c, "to")));
                var columns = Of(recs, "struct").Where(r => kids.Contains(S(r, "id"))).ToList();
                foreach (var c in columns)
                    Eq((parent + "." + name, root + "/" + rel), (S(c, "parent"), S(c, "path")), $"column {name}.{S(c, "name")} parent/path");
                Eq(cols, string.Join(",", columns.Select(c => $"{S(c, "name")}:{I(c, "start_line")}-{I(c, "end_line")}")), $"{name} columns");
            }
            Eq(20, Of(recs, "struct").Count, "struct records (objects plus columns)");
            Eq(2, Of(recs, "function").Count, "function records");
            Eq(0, recs.Count(r => S(r, "name") == "ITEMS"), "repeat spelling dropped");

            Eq("sql: scripts/b.sql:2:1: repeated declaration of dbo.Items, first declared at scripts/a.sql:1",
                string.Join("\n", errors.Split('\n', StringSplitOptions.RemoveEmptyEntries)), "diagnostics");
        }

        static void TestEmitterTargetsList()
        {
            string dir = Path.Combine(Path.GetTempPath(), "sqlfe-e2e-" + Guid.NewGuid().ToString("N"));
            try
            {
                Directory.CreateDirectory(dir);
                string root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(dir));
                foreach (var (file, table) in new[] { ("kept.sql", "Kept"), ("Upper.SQL", "Upper"), ("stray.sql", "Stray"), ("skipme.sql", "Skip") })
                    File.WriteAllText(Path.Combine(root, file), $"CREATE TABLE dbo.{table} (id INT)\n");
                var listed = new[] { "kept.sql", "Upper.SQL", "skipme.sql" }.Select(f => root + "/" + f).ToList();

                var recs = ParseLines(RunTargets(root, listed, new List<string> { "skipme" }, "n").Item1);
                Eq(string.Join(",", new[] { root + "/Upper.SQL", root + "/kept.sql" }),
                    string.Join(",", Of(recs, "file").Select(r => S(r, "path")).OrderBy(p => p, StringComparer.Ordinal)),
                    "only listed, non-excluded files emitted");
                Eq("Kept,Upper", string.Join(",", Of(recs, "struct").Where(r => S(r, "parent") == "dbo").Select(r => S(r, "name"))
                    .OrderBy(n => n, StringComparer.Ordinal)), "tables of the emitted files only");

                var empty = ParseLines(RunTargets(root, new List<string>(), new List<string>(), "n").Item1);
                Eq(0, Of(empty, "file").Count, "empty list emits no file record");
                Eq(0, Of(empty, "struct").Count, "empty list emits no struct record");

                // A UTF-8 BOM is in the file's bytes: offsets count it, lines do not.
                File.WriteAllBytes(Path.Combine(root, "bom.sql"),
                    new byte[] { 0xEF, 0xBB, 0xBF }.Concat(Encoding.UTF8.GetBytes("\nCREATE TABLE dbo.Bom (id INT)\n")).ToArray());
                var bom = ParseLines(RunTargets(root, new[] { root + "/bom.sql", root + "/kept.sql" }, new List<string>(), "n").Item1);
                var b = One(bom, "struct", "dbo", "Bom");
                Eq((3 + 1, 3 + 1 + 29, 2, 2), (I(b, "start"), I(b, "end"), I(b, "start_line"), I(b, "end_line")), "BOM file object bytes/lines");
                var k = One(bom, "struct", "dbo", "Kept");
                Eq((0, 30, 1), (I(k, "start"), I(k, "end"), I(k, "start_line")), "non-BOM file object bytes unchanged");
            }
            finally
            {
                if (Directory.Exists(dir)) Directory.Delete(dir, true);
            }
        }

        // Program.Main in-process with Console.Out/Error captured (as RunScanner in src/csharplib/tests/Program.cs).
        static (int, string, string) RunMain(params string[] args)
        {
            var outWriter = new StringWriter();
            var errWriter = new StringWriter();
            var oldOut = Console.Out;
            var oldErr = Console.Error;
            Console.SetOut(outWriter);
            Console.SetError(errWriter);
            int code;
            try { code = Apg.SqlFrontend.Program.Main(args); }
            finally
            {
                Console.SetOut(oldOut);
                Console.SetError(oldErr);
            }
            return (code, outWriter.ToString(), errWriter.ToString());
        }

        // A record with its id/from/to opaque-id prefix stripped, for comparing streams across --id-prefix values.
        static string Normalise(JsonElement r, string prefix) =>
            string.Join(",", r.EnumerateObject().Select(p =>
                (p.Name == "id" || p.Name == "from" || p.Name == "to") && p.Value.ValueKind == JsonValueKind.String
                    && Regex.IsMatch(p.Value.GetString(), "^" + Regex.Escape(prefix) + "[0-9]+$")
                    ? p.Name + "=#" + p.Value.GetString().Substring(prefix.Length)
                    : p.Name + "=" + p.Value.GetRawText()));

        static void TestEmitterCli()
        {
            string root = EmitterRoot();
            var targets = FixtureTargets(root);
            string tmp = Path.GetTempPath(), stamp = Guid.NewGuid().ToString("N");
            string listFile = Path.Combine(tmp, "sqlfe-targets-" + stamp + ".txt");
            string emptyFile = Path.Combine(tmp, "sqlfe-empty-" + stamp + ".txt");
            try
            {
                File.WriteAllLines(listFile, targets);
                var (code, output, errors) = RunMain(root, "--targets", listFile, "--id-prefix", "q");
                Eq(0, code, "exit code");
                var lines = output.Split('\n', StringSplitOptions.RemoveEmptyEntries);
                foreach (var line in lines)
                {
                    bool isObject;
                    try { using var doc = JsonDocument.Parse(line); isObject = doc.RootElement.ValueKind == JsonValueKind.Object; }
                    catch (JsonException) { isObject = false; }
                    Eq(true, isObject, $"stdout line is a JSON object: {line}");
                }
                var recs = ParseLines(output);
                var modules = Of(recs, "module").Select(r => S(r, "fqn")).ToList();
                foreach (var proj in targets.Where(t => t.EndsWith(".sqlproj", StringComparison.OrdinalIgnoreCase)))
                    Eq(1, modules.Count(m => m == proj), $"module record for {proj}");
                foreach (var m in modules)
                    Eq(true, m == "dbo" || m.StartsWith(root + "/", StringComparison.Ordinal), $"module <{m}> absolute or bare schema");
                var sqlFiles = targets.Where(t => t.EndsWith(".sql", StringComparison.OrdinalIgnoreCase)).ToList();
                Eq(sqlFiles.Count, Of(recs, "file").Count, "file records");
                foreach (var f in sqlFiles)
                    Eq(1, Of(recs, "file").Count(r => S(r, "path") == f), $"file record for {f}");
                foreach (var r in recs.Where(r => S(r, "id") != null))
                    Eq(true, S(r, "id").StartsWith("q", StringComparison.Ordinal), $"id {S(r, "id")} has prefix q");

                var (direct, directErrors) = RunTargets(root, targets, new List<string>(), "n");
                Eq(string.Join("\n", ParseLines(direct).Select(r => Normalise(r, "n"))),
                    string.Join("\n", recs.Select(r => Normalise(r, "q"))), "CLI records equal SchemaEmitter.Run modulo id prefix");
                Eq(directErrors, errors, "CLI diagnostics equal SchemaEmitter.Run");

                File.WriteAllText(emptyFile, "");
                var (emptyCode, emptyOut, _) = RunMain(root, "--targets", emptyFile, "--id-prefix", "q");
                Eq(0, emptyCode, "empty targets exit code");
                Eq(0, Of(ParseLines(emptyOut), "file").Count, "empty targets emits no file record");
            }
            finally
            {
                File.Delete(listFile);
                File.Delete(emptyFile);
            }
        }
    }
}
