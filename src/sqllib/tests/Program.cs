using System;
using System.Collections.Generic;
using System.Linq;
using Apg.SqlFrontend;
using Microsoft.SqlServer.TransactSql.ScriptDom;

namespace SqlFrontend.Tests
{
    /// Framework-free gate tests for the SQL frontend, run with `dotnet run` and this
    /// project's path (src/sqllib/tests/SqlFrontend.Tests.csproj). Prints FAIL lines and
    /// exits non-zero on any failure. Every test here is pure in-memory.
    public static class TestMain
    {
        static int _failures;
        static int _checks;

        public static int Main()
        {
            var tests = new List<(string, Action)>
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
            };
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
    }
}
