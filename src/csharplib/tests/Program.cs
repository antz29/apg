using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Text;
using System.Text.Json;
using Microsoft.CodeAnalysis;

namespace CsharpFrontend.Tests;

/// <summary>
/// Gate tests for the phase-02 task-12 targeted (file-granularity) C# scan.
///
/// The load-bearing invariant: a symbol declared in a NON-emitted file must be
/// emitted as the ingestor's canonical FQN, so the cached unit supplies the
/// endpoint node and the cross-file `calls`/`uses` edge survives
/// `apg::ingest::finalize_graph`'s dangling-edge pruning — i.e. the targeted
/// graph equals a full scan (requirements.requirement.exact-ongoing-equivalence
/// and requirements.requirement.full-context-targeted-emission).
///
/// Covered directly: Scanner.TryResolveEndpoint -> Scanner.CanonicalFqn ->
/// Scanner.EmittedMethodName, and Scanner.RegisterUniverse's overload-group
/// counts, for the struct / constructor / property-accessor / overloaded-method
/// canonical forms. A wrong form would be silently pruned downstream, so the
/// assertions are made against the SAME rendering rule the Rust ingestor uses
/// (src/ingest.rs render_function_fqns / struct rendering, re-implemented here
/// as an independent oracle over the full-scan output).
///
/// Run (framework-free, like the sibling Java frontend test), using `dotnet run`
/// with the tests project path (src/csharplib/tests/CsharpFrontend.Tests.csproj).
/// Exits non-zero and prints FAIL lines on any divergence.
/// </summary>
public static class TestMain
{
    private static int _failures;

    public static int Main()
    {
        string baseDir = Path.Combine(Path.GetTempPath(), "apg-csharp-test-" + Guid.NewGuid().ToString("N"));
        try
        {
            string proj = Path.GetFullPath(Path.Combine(baseDir, "proj"));
            WriteFixture(proj);

            TestFullScanEqualsAllTargets(proj, baseDir);
            TestEmptyTargetsMeansNoFilter(proj, baseDir);
            TestMissingTargetsMeansNoFilter(proj, baseDir);
            TestTargetedOmitsNonTargetDeclarations(proj, baseDir);
            TestCrossFileEndpointsAreCanonical(proj, baseDir);
            TestTargetedEqualsFullForTargetFile(proj, baseDir);
            TestCanonicalFormsDirectly(proj);
        }
        catch (Exception ex)
        {
            _failures++;
            Console.WriteLine("FAIL: unhandled exception");
            Console.WriteLine(ex);
        }
        finally
        {
            try { if (Directory.Exists(baseDir)) Directory.Delete(baseDir, true); } catch { /* best effort */ }
        }

        if (_failures == 0)
        {
            Console.WriteLine("ALL TESTS PASSED");
            return 0;
        }
        Console.WriteLine($"{_failures} TEST(S) FAILED");
        return 1;
    }

    // ------------------------------------------------------------------
    // Fixture: one project, two files. A.cs is the only target; every symbol
    // it references that is declared ONLY in B.cs must come out as a canonical
    // FQN. B declares an explicit ctor, an auto-property (get/set accessors),
    // an overloaded method group, and a nested type.
    // ------------------------------------------------------------------

    private static void WriteFixture(string proj)
    {
        Directory.CreateDirectory(proj);
        File.WriteAllText(Path.Combine(proj, "B.cs"), """
            namespace App;

            public class B
            {
                public B() { }
                public int Prop { get; set; }
                public void Plain() { }
                public int Over(int x) { return x; }
                public string Over(string s) { return s; }

                public class Nested { }
            }
            """, new UTF8Encoding(false));
        File.WriteAllText(Path.Combine(proj, "A.cs"), """
            namespace App;

            public class A
            {
                public B Field;
                public B.Nested NestedField;

                public void Run()
                {
                    B b = new B();
                    b.Prop = 1;
                    int p = b.Prop;
                    b.Plain();
                    int x = b.Over(2);
                    string s = b.Over("hi");
                    NestedField = new B.Nested();
                }
            }
            """, new UTF8Encoding(false));
    }

    private static string APath(string proj) => Path.GetFullPath(Path.Combine(proj, "A.cs"));
    private static string BPath(string proj) => Path.GetFullPath(Path.Combine(proj, "B.cs"));

    private static IEnumerable<string> AllCsFiles(string proj)
        => Directory.GetFiles(proj, "*.cs", SearchOption.AllDirectories)
                    .Select(Path.GetFullPath)
                    .OrderBy(p => p, StringComparer.Ordinal);

    private static string WriteTargets(string baseDir, string name, params string[] files)
    {
        var path = Path.Combine(baseDir, name);
        File.WriteAllLines(path, files);
        return path;
    }

    /// Runs the frontend in-process with stdout/stderr captured (mirrors the
    /// Java frontend test's redirect-and-call-main helper).
    private static (int Code, string Out, string Err) RunScanner(string proj, params string[] extra)
    {
        var args = new List<string> { proj };
        args.AddRange(extra);
        var outWriter = new StringWriter();
        var errWriter = new StringWriter();
        var oldOut = Console.Out;
        var oldErr = Console.Error;
        Console.SetOut(outWriter);
        Console.SetError(errWriter);
        int code;
        try
        {
            code = Apg.CsharpFrontend.Program.Main(args.ToArray());
        }
        finally
        {
            Console.SetOut(oldOut);
            Console.SetError(oldErr);
        }
        return (code, outWriter.ToString(), errWriter.ToString());
    }

    // ------------------------------------------------------------------
    // Tests
    // ------------------------------------------------------------------

    /// <summary>An all-targets request is byte-identical to a plain full scan.</summary>
    private static void TestFullScanEqualsAllTargets(string proj, string baseDir)
    {
        var full = RunScanner(proj);
        var allTargets = WriteTargets(baseDir, "all.targets", AllCsFiles(proj).ToArray());
        var targeted = RunScanner(proj, "--targets", allTargets);
        Check("all-targets scan is byte-identical to the full scan",
            full.Out == targeted.Out, Diff(full.Out, targeted.Out) + "\nSTDERR:\n" + targeted.Err);
    }

    /// <summary>An empty targets file means NO filter (the pinned hand-off: absent/empty = full).</summary>
    private static void TestEmptyTargetsMeansNoFilter(string proj, string baseDir)
    {
        var full = RunScanner(proj).Out;
        var empty = Path.Combine(baseDir, "empty.targets");
        File.WriteAllText(empty, "");
        var r = RunScanner(proj, "--targets", empty);
        Check("an empty --targets file is byte-identical to the full scan",
            full == r.Out, Diff(full, r.Out) + "\nSTDERR:\n" + r.Err);
    }

    /// <summary>A missing targets file warns and means NO filter, never "emit nothing".</summary>
    private static void TestMissingTargetsMeansNoFilter(string proj, string baseDir)
    {
        var full = RunScanner(proj).Out;
        var missing = Path.Combine(baseDir, "absent.targets");
        var r = RunScanner(proj, "--targets", missing);
        Check("a missing --targets file is byte-identical to the full scan",
            full == r.Out, Diff(full, r.Out) + "\nSTDERR:\n" + r.Err);
    }

    /// <summary>With a target set in force, only the target file's declarations are emitted.</summary>
    private static void TestTargetedOmitsNonTargetDeclarations(string proj, string baseDir)
    {
        var targets = WriteTargets(baseDir, "one.targets", APath(proj));
        var r = RunScanner(proj, "--targets", targets);
        Check("targeted scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        var c = Canonicalize(ParseGraph(r.Out));

        bool emitsB = c.Records.Any(s => IsDecl(s) && RecordPath(s).StartsWith(BPath(proj), StringComparison.Ordinal));
        Check("targeted scan omits the non-target file's struct/function declarations",
            !emitsB, Join(c.Records));

        bool emitsA = c.Records.Any(s => IsDecl(s) && RecordPath(s).StartsWith(APath(proj), StringComparison.Ordinal));
        Check("targeted scan emits the target file's declarations", emitsA, Join(c.Records));
    }

    /// <summary>
    /// The core exactness property: a target file's edge to a symbol declared
    /// in a non-emitted file carries that symbol's canonical FQN, and every
    /// such endpoint is a node a full scan declares (so finalize_graph keeps
    /// it, never prunes it as dangling).
    /// </summary>
    private static void TestCrossFileEndpointsAreCanonical(string proj, string baseDir)
    {
        var targets = WriteTargets(baseDir, "a.targets", APath(proj));
        var r = RunScanner(proj, "--targets", targets);
        var g = ParseGraph(r.Out);
        var c = Canonicalize(g);
        var full = Canonicalize(ParseGraph(RunScanner(proj).Out));

        var want = new[]
        {
            "uses|App.A|App.B",                       // struct    -> parent.name
            "uses|App.A.Run|App.B",                   // struct (from a local type use)
            "uses|App.A|App.B.Nested",                // nested type -> parent.name
            "calls|App.A.Run|App.B.B",                // ctor      -> parent.<TypeSimpleName>
            "calls|App.A.Run|App.B.Plain",            // unique method -> parent.name
            "calls|App.A.Run|App.B.Over(int)",        // overloaded -> parent.name(T1,T2)
            "calls|App.A.Run|App.B.Over(string)",
            "unresolved_call|App.A.Run|App.B.Nested..ctor", // implicit ctor stays unresolved (both scans)
        };
        foreach (var w in want)
        {
            Check($"targeted cross-file endpoint is canonical: {w}",
                c.Records.Contains(w), Join(c.Records) + "\nSTDERR:\n" + r.Err);
        }

        // No dangling endpoints: every resolved calls/uses `to` is either an
        // emitted local id or a canonical FQN a full scan declares.
        foreach (var edge in g.Edges.Where(e => e.Type is "calls" or "uses"))
        {
            if (c.IdToFqn.ContainsKey(edge.To)) continue;
            Check($"targeted endpoint resolves into the full-scan node set: {edge.Type} {edge.From} -> {edge.To}",
                full.NodeFqns.Contains(edge.To),
                $"`{edge.To}` is neither an emitted id nor a full-scan canonical FQN; finalize_graph would prune it.");
        }
    }

    /// <summary>
    /// Targeted facts for the target file equal what a full scan emits for that
    /// file (same records, ids canonicalized), mirroring the phase-02 int test's
    /// oracle (src/main.rs acceptance_targeted_rescan_equivalence_*).
    /// </summary>
    private static void TestTargetedEqualsFullForTargetFile(string proj, string baseDir)
    {
        var full = Canonicalize(ParseGraph(RunScanner(proj).Out));
        var targets = WriteTargets(baseDir, "eq.targets", APath(proj));
        var targeted = Canonicalize(ParseGraph(RunScanner(proj, "--targets", targets).Out));

        var expected = FilterToTarget(full, APath(proj));
        Check("targeted facts for the target file equal the full scan's target-file facts",
            expected.SetEquals(targeted.Records), SetDiff(expected, targeted.Records));
    }

    /// <summary>
    /// Direct coverage of the canonical forms the cross-file path computes.
    /// Property accessors cannot be an edge endpoint (the scanner emits no edge
    /// for a property read/write), so the accessor form is asserted by invoking
    /// Scanner.CanonicalFqn on the accessor symbols registered for the
    /// non-emitted file — the exact function TryResolveEndpoint delegates to.
    /// </summary>
    private static void TestCanonicalFormsDirectly(string proj)
    {
        var scanner = new Apg.CsharpFrontend.Scanner(
            proj,
            new List<string>(),
            new List<string>(),
            "n",
            new HashSet<string>(StringComparer.Ordinal) { APath(proj) });

        // Run() populates _projectSymbols and _methodGroups via RegisterUniverse
        // (the filtered path). Capture/discard its console output.
        var oldOut = Console.Out;
        var oldErr = Console.Error;
        Console.SetOut(TextWriter.Null);
        Console.SetError(TextWriter.Null);
        try { scanner.Run(); }
        finally { Console.SetOut(oldOut); Console.SetError(oldErr); }

        var flags = BindingFlags.NonPublic | BindingFlags.Instance;
        var symbolsField = typeof(Apg.CsharpFrontend.Scanner).GetField("_projectSymbols", flags)!;
        var canonicalMethod = typeof(Apg.CsharpFrontend.Scanner).GetMethod("CanonicalFqn", flags)!;
        var symbols = (IEnumerable<ISymbol>)symbolsField.GetValue(scanner)!;

        var forms = new SortedSet<string>(StringComparer.Ordinal);
        foreach (var s in symbols)
        {
            if (s is IMethodSymbol || s is INamedTypeSymbol)
            {
                forms.Add((string)canonicalMethod.Invoke(scanner, new object[] { s })!);
            }
        }

        Check("canonical struct form for a non-emitted symbol: App.B", forms.Contains("App.B"), Join(forms));
        Check("canonical nested-type form for a non-emitted symbol: App.B.Nested",
            forms.Contains("App.B.Nested"), Join(forms));
        Check("canonical ctor form for a non-emitted symbol: App.B.B", forms.Contains("App.B.B"), Join(forms));
        Check("canonical accessor form for a non-emitted symbol: App.B.get_Prop",
            forms.Contains("App.B.get_Prop"), Join(forms));
        Check("canonical accessor form for a non-emitted symbol: App.B.set_Prop",
            forms.Contains("App.B.set_Prop"), Join(forms));
        Check("canonical overloaded form for a non-emitted symbol: App.B.Over(int)",
            forms.Contains("App.B.Over(int)"), Join(forms));
        Check("canonical overloaded form for a non-emitted symbol: App.B.Over(string)",
            forms.Contains("App.B.Over(string)"), Join(forms));
    }

    // ------------------------------------------------------------------
    // Record parsing / canonicalization (the ingestor oracle, re-implemented
    // independently from src/ingest.rs render_function_fqns + struct render).
    // ------------------------------------------------------------------

    private sealed class RawStruct
    {
        public string Id = "", Parent = "", Name = "", Path = "";
        public int Start, End, StartLine, EndLine;
    }

    private sealed class RawFunc
    {
        public string Id = "", Parent = "", Name = "", File = "";
        public List<string> Params = new();
        public int Start, End, StartLine, EndLine;
    }

    private sealed class RawEdge { public string Type = "", From = "", To = ""; }
    private sealed class RawUnres { public string Fqn = "", Category = ""; }

    private sealed class RawGraph
    {
        public List<string> Modules = new();
        public List<string> Files = new(); // "path|parent|start_line|end_line"
        public List<RawStruct> Structs = new();
        public List<RawFunc> Funcs = new();
        public List<RawEdge> Edges = new();
        public List<RawUnres> Unres = new();
    }

    private sealed class Canonical
    {
        public Dictionary<string, string> IdToFqn = new(StringComparer.Ordinal);
        public SortedSet<string> Records = new(StringComparer.Ordinal);
        public HashSet<string> NodeFqns = new(StringComparer.Ordinal);
        public HashSet<string> UnresFqns = new(StringComparer.Ordinal);
        public HashSet<string> ModuleFqns = new(StringComparer.Ordinal);
    }

    private static RawGraph ParseGraph(string raw)
    {
        var g = new RawGraph();
        foreach (var line in raw.Split('\n'))
        {
            if (line.Length == 0) continue;
            using var doc = JsonDocument.Parse(line);
            var e = doc.RootElement;
            var type = Str(e, "type");
            switch (type)
            {
                case "module":
                    g.Modules.Add(Str(e, "fqn"));
                    break;
                case "file":
                    g.Files.Add($"{Str(e, "path")}|{Str(e, "parent")}|{Num(e, "start_line")}|{Num(e, "end_line")}");
                    break;
                case "struct":
                    g.Structs.Add(new RawStruct
                    {
                        Id = Str(e, "id"), Parent = Str(e, "parent"), Name = Str(e, "name"), Path = Str(e, "path"),
                        Start = Num(e, "start"), End = Num(e, "end"),
                        StartLine = Num(e, "start_line"), EndLine = Num(e, "end_line"),
                    });
                    break;
                case "function":
                    g.Funcs.Add(new RawFunc
                    {
                        Id = Str(e, "id"), Parent = Str(e, "parent"), Name = Str(e, "name"), File = Str(e, "file"),
                        Params = StrArray(e, "params"),
                        Start = Num(e, "start"), End = Num(e, "end"),
                        StartLine = Num(e, "start_line"), EndLine = Num(e, "end_line"),
                    });
                    break;
                case "contains":
                case "calls":
                case "uses":
                case "unresolved_call":
                case "unresolved_use":
                    g.Edges.Add(new RawEdge { Type = type, From = Str(e, "from"), To = Str(e, "to") });
                    break;
                case "unresolved":
                    g.Unres.Add(new RawUnres { Fqn = Str(e, "fqn"), Category = Str(e, "category") });
                    break;
            }
        }
        return g;
    }

    private static Canonical Canonicalize(RawGraph g)
    {
        var c = new Canonical();

        foreach (var m in g.Modules) { c.Records.Add("module|" + m); c.NodeFqns.Add(m); c.ModuleFqns.Add(m); }
        foreach (var f in g.Files) c.Records.Add("file|" + f);

        foreach (var s in g.Structs)
        {
            string fqn = string.IsNullOrEmpty(s.Parent) ? s.Name : s.Parent + "." + s.Name;
            c.IdToFqn[s.Id] = fqn;
            c.NodeFqns.Add(fqn);
            c.Records.Add($"struct|{fqn}|{s.Path}|{s.Start}|{s.End}|{s.StartLine}|{s.EndLine}");
        }

        // Functions group by (parent,name): singleton -> parent.name,
        // overloaded group -> parent.name(T1,T2,...). This is exactly the
        // ingestor's rendering (src/ingest.rs render_function_fqns).
        var groups = new Dictionary<string, List<RawFunc>>(StringComparer.Ordinal);
        foreach (var f in g.Funcs)
        {
            string key = f.Parent + "\u0000" + f.Name;
            if (!groups.TryGetValue(key, out var list)) { list = new List<RawFunc>(); groups[key] = list; }
            list.Add(f);
        }
        foreach (var list in groups.Values)
        {
            foreach (var f in list)
            {
                string name = list.Count == 1 ? f.Name : $"{f.Name}({string.Join(",", f.Params)})";
                string fqn = string.IsNullOrEmpty(f.Parent) ? name : f.Parent + "." + name;
                c.IdToFqn[f.Id] = fqn;
                c.NodeFqns.Add(fqn);
                c.Records.Add($"function|{fqn}|{f.File}|{f.Start}|{f.End}|{f.StartLine}|{f.EndLine}|{string.Join(",", f.Params)}");
            }
        }

        foreach (var e in g.Edges)
        {
            string from = c.IdToFqn.TryGetValue(e.From, out var f) ? f : e.From;
            string to = c.IdToFqn.TryGetValue(e.To, out var t) ? t : e.To;
            c.Records.Add($"{e.Type}|{from}|{to}");
        }
        foreach (var u in g.Unres) { c.Records.Add($"unresolved|{u.Fqn}|{u.Category}"); c.UnresFqns.Add(u.Fqn); }

        return c;
    }

    /// <summary>
    /// Keeps only the records a targeted scan of `targetFile` should emit.
    /// Module scaffolding is global (Scanner.Run's doc comment: a non-target
    /// file still contributes its module hierarchy), so ALL module records and
    /// module-to-module contains edges are kept; only per-file declarations and
    /// the edges they author are restricted to the target file.
    /// </summary>
    private static SortedSet<string> FilterToTarget(Canonical all, string targetFile)
    {
        var targetUnits = new HashSet<string>(StringComparer.Ordinal);
        foreach (var rec in all.Records)
        {
            var p = rec.Split('|');
            if ((p[0] == "struct" || p[0] == "function") && p[2] == targetFile) targetUnits.Add(p[1]);
        }

        var keep = new SortedSet<string>(StringComparer.Ordinal);
        var keptUnres = new HashSet<string>(StringComparer.Ordinal);

        foreach (var rec in all.Records)
        {
            var p = rec.Split('|');
            switch (p[0])
            {
                case "module":
                    keep.Add(rec);
                    break;
                case "file":
                    if (p[1] == targetFile) keep.Add(rec);
                    break;
                case "struct":
                case "function":
                    if (p[2] == targetFile) keep.Add(rec);
                    break;
                case "contains":
                    if ((all.ModuleFqns.Contains(p[1]) && all.ModuleFqns.Contains(p[2])) ||
                        (targetUnits.Contains(p[1]) && targetUnits.Contains(p[2]))) keep.Add(rec);
                    break;
                case "calls":
                case "uses":
                case "unresolved_call":
                case "unresolved_use":
                    if (targetUnits.Contains(p[1]))
                    {
                        keep.Add(rec);
                        if (p[0].StartsWith("unresolved", StringComparison.Ordinal)) keptUnres.Add(p[2]);
                    }
                    break;
            }
        }

        foreach (var rec in all.Records)
        {
            var p = rec.Split('|');
            if (p[0] == "unresolved" && keptUnres.Contains(p[1])) keep.Add(rec);
        }
        return keep;
    }

    // ------------------------------------------------------------------
    // Small helpers
    // ------------------------------------------------------------------

    private static bool IsDecl(string rec)
        => rec.StartsWith("struct|", StringComparison.Ordinal) || rec.StartsWith("function|", StringComparison.Ordinal);

    private static string RecordPath(string rec) => rec.Split('|')[2];

    private static string Str(JsonElement e, string key)
        => e.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() ?? "" : "";

    private static int Num(JsonElement e, string key)
        => e.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.Number ? v.GetInt32() : 0;

    private static List<string> StrArray(JsonElement e, string key)
    {
        var list = new List<string>();
        if (e.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.Array)
        {
            foreach (var item in v.EnumerateArray()) list.Add(item.GetString() ?? "");
        }
        return list;
    }

    private static void Check(string name, bool ok, string detail)
    {
        if (ok) { Console.WriteLine("PASS: " + name); return; }
        _failures++;
        Console.WriteLine("FAIL: " + name);
        Console.WriteLine(detail);
    }

    private static string Join(IEnumerable<string> items) => string.Join("\n", items);

    private static string SetDiff(IEnumerable<string> a, IEnumerable<string> b)
    {
        var sa = new SortedSet<string>(a, StringComparer.Ordinal);
        var sb = new SortedSet<string>(b, StringComparer.Ordinal);
        var onlyA = new SortedSet<string>(sa, StringComparer.Ordinal); onlyA.ExceptWith(sb);
        var onlyB = new SortedSet<string>(sb, StringComparer.Ordinal); onlyB.ExceptWith(sa);
        return "only in full-for-target:\n" + Join(onlyA) + "\nonly in targeted:\n" + Join(onlyB);
    }

    private static string Diff(string a, string b) => SetDiff(a.Split('\n'), b.Split('\n'));
}
