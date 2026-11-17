using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
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
                ["int"] = new List<(string, Action)>(),
                ["e2e"] = new List<(string, Action)> { ("ProjectFixtureScopes", TestProjectFixtureScopes) },
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
    }
}
