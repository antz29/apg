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
            TestDiscoverFilesClaimsAllExtensions(baseDir);
            TestParseTreesKindsAndHoldBack(baseDir);
            TestHeldBackFilesAreFileOnly(baseDir);
            TestMixedCsCsxScan(baseDir);
            TestScriptMemberScope(baseDir);
            TestProjectSdkStyle(baseDir);
            TestProjectSdkElementNoDefaultItems(baseDir);
            TestProjectFrameworkStyle(baseDir);
            TestSolutionReader(baseDir);
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
    // Source kinds: .cs / .csx / .cshtml / .razor
    // ------------------------------------------------------------------

    // SourceDiscovery and CompilationLoader are internal (no InternalsVisibleTo),
    // so they are reached by reflection like the private members above.
    private static Type FrontendType(string name)
        => typeof(Apg.CsharpFrontend.Scanner).Assembly.GetType("Apg.CsharpFrontend." + name, throwOnError: true)!;

    private static string WriteSource(string dir, string name, string text)
    {
        Directory.CreateDirectory(dir);
        var path = Path.GetFullPath(Path.Combine(dir, name));
        File.WriteAllText(path, text, new UTF8Encoding(false));
        return path;
    }

    /// <summary>DiscoverFiles claims .cs/.csx/.cshtml/.razor case-insensitively and nothing else.</summary>
    private static void TestDiscoverFilesClaimsAllExtensions(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "discover"));
        var want = new SortedSet<string>(StringComparer.Ordinal)
        {
            WriteSource(dir, "A.cs", "class A { }"),
            WriteSource(dir, "B.CSX", "int x = 1;"),
            WriteSource(dir, "C.cshtml", "<p>c</p>"),
            WriteSource(dir, "D.Razor", "<p>d</p>"),
        };
        WriteSource(dir, "E.txt", "not source");
        WriteSource(dir, "F.csproj", "<Project />");

        var discovery = Activator.CreateInstance(FrontendType("SourceDiscovery"), new List<string>(), new List<string>())!;
        var found = (IEnumerable<string>)discovery.GetType().GetMethod("DiscoverFiles")!.Invoke(discovery, new object[] { dir })!;
        var got = new SortedSet<string>(found, StringComparer.Ordinal);
        Check("DiscoverFiles claims exactly the .cs/.csx/.cshtml/.razor files (case-insensitive)",
            want.SetEquals(got), SetDiff(want, got));
    }

    /// <summary>ParseTrees parses .csx as Script, .cs as Regular, and holds back .cshtml/.razor unparsed.</summary>
    private static void TestParseTreesKindsAndHoldBack(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "parse"));
        var cs = WriteSource(dir, "A.cs", "class A { }");
        var csx = WriteSource(dir, "S.csx", "void Hello() { }");
        var cshtml = WriteSource(dir, "V.cshtml", "<p>v</p>");
        var razor = WriteSource(dir, "C.razor", "<p>c</p>");

        var args = new object?[] { new List<string> { cs, csx, cshtml, razor }, null, null };
        var trees = (List<SyntaxTree>)FrontendType("CompilationLoader").GetMethod("ParseTrees")!.Invoke(null, args)!;
        var heldBack = (List<string>)args[1]!;

        var kinds = trees.ToDictionary(t => t.FilePath, t => t.Options.Kind);
        Check("ParseTrees parses exactly the .cs and .csx files",
            kinds.Count == 2 && kinds.ContainsKey(cs) && kinds.ContainsKey(csx), Join(kinds.Keys));
        Check("ParseTrees parses .cs as SourceCodeKind.Regular",
            kinds.TryGetValue(cs, out var k1) && k1 == SourceCodeKind.Regular, k1.ToString());
        Check("ParseTrees parses .csx as SourceCodeKind.Script",
            kinds.TryGetValue(csx, out var k2) && k2 == SourceCodeKind.Script, k2.ToString());
        Check("ParseTrees holds back .cshtml/.razor in discovery order",
            heldBack.SequenceEqual(new[] { cshtml, razor }), Join(heldBack));
    }

    /// <summary>
    /// A held-back .cshtml/.razor file is emitted as a file record only (root
    /// parent, 1..line-count) with no declarations, and honours the target filter.
    /// </summary>
    private static void TestHeldBackFilesAreFileOnly(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "heldback"));
        var cs = WriteSource(dir, "A.cs", "namespace App;\n\npublic class A { }\n");
        var cshtml = WriteSource(dir, "V.cshtml", "@page\n<h1>Hi</h1>\n@code { void M() { } }");
        var razor = WriteSource(dir, "C.razor", "<p>one</p>\n<p>two</p>");

        var r = RunScanner(dir);
        Check("held-back scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        var c = Canonicalize(ParseGraph(r.Out));
        Check("held-back .cshtml emits a file-only record (root parent, 1..3)",
            c.Records.Contains($"file|{cshtml}||1|3"), Join(c.Records));
        Check("held-back .razor emits a file-only record (root parent, 1..2)",
            c.Records.Contains($"file|{razor}||1|2"), Join(c.Records));
        Check("held-back files emit no struct/function declarations",
            !c.Records.Any(s => IsDecl(s) && (RecordPath(s) == cshtml || RecordPath(s) == razor)), Join(c.Records));
        Check("the .cs file beside held-back files still emits its declarations",
            c.Records.Any(s => IsDecl(s) && RecordPath(s) == cs), Join(c.Records));

        var targets = WriteTargets(baseDir, "heldback.targets", cs);
        var t = Canonicalize(ParseGraph(RunScanner(dir, "--targets", targets).Out));
        Check("a target filter omits non-target held-back file records",
            !t.Records.Any(s => s.StartsWith("file|", StringComparison.Ordinal) && (s.Contains(cshtml) || s.Contains(razor))),
            Join(t.Records));
    }

    /// <summary>
    /// A mixed .cs/.csx tree: the .cs files share one compilation and the .csx
    /// compiles in its own; the .csx File
    /// record carries its top-level method's declaration, and the .cs
    /// declarations are exactly those of a .cs-only scan.
    /// </summary>
    private static void TestMixedCsCsxScan(string baseDir)
    {
        const string csText = "namespace App;\n\npublic class A\n{\n    public void Run() { }\n}\n";
        var csOnlyDir = Path.GetFullPath(Path.Combine(baseDir, "mixed-cs-only"));
        var mixedDir = Path.GetFullPath(Path.Combine(baseDir, "mixed"));
        var csOnly = WriteSource(csOnlyDir, "A.cs", csText);
        var cs = WriteSource(mixedDir, "A.cs", csText);
        var csx = WriteSource(mixedDir, "S.csx", "int Twice(int x) { return x * 2; }\nvar y = Twice(2);\n");

        var r = RunScanner(mixedDir);
        Check("mixed .cs/.csx scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        var g = ParseGraph(r.Out);
        var c = Canonicalize(g);
        var all = Join(c.Records) + "\nSTDERR:\n" + r.Err;

        Check("mixed scan emits a script module record whose identity is the S.csx path",
            g.Modules.Contains(csx), Join(g.Modules));
        Check("mixed scan's .csx File record parent is the S.csx script module",
            g.Files.Any(f => f.StartsWith($"{csx}|{csx}|", StringComparison.Ordinal)), Join(g.Files));
        Check("mixed scan's .csx top-level method FQN is exactly <S.csx path>.Twice",
            c.Records.Any(s => s.StartsWith($"function|{csx}.Twice|{csx}|", StringComparison.Ordinal)), all);

        var baseline = Canonicalize(ParseGraph(RunScanner(csOnlyDir).Out)).Records
            .Where(IsDecl).Select(s => s.Replace(csOnly, cs)).ToList();
        var mixedCs = c.Records.Where(s => IsDecl(s) && RecordPath(s) == cs).ToList();
        Check("mixed scan emits the .cs declarations unchanged",
            baseline.Count > 0 && new SortedSet<string>(baseline, StringComparer.Ordinal).SetEquals(mixedCs),
            SetDiff(baseline, mixedCs));
    }

    /// <summary>
    /// Script member scope: each .csx is one whole-path module, its members hang
    /// under that path (distinct per script), calls stay within their own script,
    /// a targeted scan renders the same FQNs, and a .cs namespace sharing the
    /// dotted spelling (tools.build) is unaffected.
    /// </summary>
    private static void TestScriptMemberScope(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "script-scope"));
        const string body = "void Log(string msg) { }\nvoid Run() { Log(\"x\"); }\n";
        var p1 = WriteSource(Path.Combine(dir, "tools"), "build.csx",
            body + "class Opts { public int N { get; set; } class Inner { } }\n");
        var p2 = WriteSource(Path.Combine(dir, "scripts", "other"), "build.csx",
            body + "class Opts { public int N { get; set; } }\nvoid Make() { var l = new tools.build.Log(); }\n");
        var cs = WriteSource(dir, "Lib.cs",
            "namespace tools.build;\n\npublic class Log { }\n\npublic class Caller { void Go() { var o = new Opts(); Log(\"x\"); } }\n");

        var r = RunScanner(dir);
        Check("script-scope scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        var g = ParseGraph(r.Out);
        var c = Canonicalize(g);
        var all = Join(c.Records) + "\nSTDERR:\n" + r.Err;

        var wantModules = new SortedSet<string>(StringComparer.Ordinal) { p1, p2, "tools", "tools.build" };
        Check("script-scope modules are exactly the two whole-path script modules plus tools/tools.build (no fragments)",
            wantModules.SetEquals(g.Modules), SetDiff(wantModules, g.Modules));
        Check("no duplicate module record", g.Modules.Count == g.Modules.Distinct(StringComparer.Ordinal).Count(), Join(g.Modules));

        foreach (var p in new[] { p1, p2 })
        {
            Check($"script File record parent is its script module: {p}",
                g.Files.Any(f => f.StartsWith($"{p}|{p}|", StringComparison.Ordinal)), Join(g.Files));
            Check($"script Log is a member of its own script: {p}.Log",
                c.Records.Any(s => s.StartsWith($"function|{p}.Log|{p}|", StringComparison.Ordinal)), all);
            Check($"script Run calls its own script's Log: {p}",
                c.Records.Contains($"calls|{p}.Run|{p}.Log"), all);
        }
        Check("no cross-script Run -> Log call",
            !c.Records.Contains($"calls|{p1}.Run|{p2}.Log") && !c.Records.Contains($"calls|{p2}.Run|{p1}.Log"), all);

        var funcFqns = c.Records.Where(s => s.StartsWith("function|", StringComparison.Ordinal)).Select(s => s.Split('|')[1]).ToList();
        var structFqns = c.Records.Where(s => s.StartsWith("struct|", StringComparison.Ordinal)).Select(s => s.Split('|')[1]).ToList();
        Check("no duplicate function FQN", funcFqns.Count == g.Funcs.Count && funcFqns.Distinct().Count() == funcFqns.Count, Join(funcFqns));
        Check("no duplicate struct FQN", structFqns.Count == g.Structs.Count && structFqns.Distinct().Count() == structFqns.Count, Join(structFqns));

        Check("script type is emitted under its script: <script>.Opts", structFqns.Contains($"{p1}.Opts"), all);
        Check("script nested type is emitted as <script>.Opts.Inner", structFqns.Contains($"{p1}.Opts.Inner"), all);
        Check("script type accessors are under <script>.Opts",
            funcFqns.Contains($"{p1}.Opts.get_N") && funcFqns.Contains($"{p1}.Opts.set_N"), all);

        // One compilation per script: the same-named Opts in each script is its own type.
        Check("each script emits its own Opts struct record",
            structFqns.Count(f => f == $"{p1}.Opts") == 1 && structFqns.Count(f => f == $"{p2}.Opts") == 1, all);
        var accessors = new[] { $"{p1}.Opts.get_N", $"{p1}.Opts.set_N", $"{p2}.Opts.get_N", $"{p2}.Opts.set_N" };
        Check("four distinct Opts accessor FQNs, one per script and accessor",
            accessors.All(a => funcFqns.Count(f => f == a) == 1), all);
        Check("Opts.Inner exists only under tools/build.csx",
            structFqns.Where(f => f.EndsWith(".Opts.Inner", StringComparison.Ordinal)).SequenceEqual(new[] { $"{p1}.Opts.Inner" }), all);
        Check("no duplicate module/file FQN",
            g.Modules.Distinct().Count() == g.Modules.Count && g.Files.Select(f => f.Split('|')[0]).Distinct().Count() == g.Files.Count, all);

        bool ResolvedEdge(string from, Func<string, bool> to) => c.Records.Any(s =>
        {
            var x = s.Split('|');
            return (x[0] == "calls" || x[0] == "uses") && x[1] == from && to(x[2]);
        });
        Check("a script binds only to itself: no resolved edge from <other script>.Make to tools.build.Log or its ctor",
            c.NodeFqns.Contains($"{p2}.Make") && !ResolvedEdge($"{p2}.Make", t => t.StartsWith("tools.build.Log", StringComparison.Ordinal)), all);
        Check(".cs code binds to no script member: no resolved edge from tools.build.Caller.Go to a script record",
            c.NodeFqns.Contains("tools.build.Caller.Go")
            && !ResolvedEdge("tools.build.Caller.Go", t => t.StartsWith(p1, StringComparison.Ordinal) || t.StartsWith(p2, StringComparison.Ordinal)), all);

        Check(".cs namespace chain tools -> tools.build is kept", c.Records.Contains("contains|tools|tools.build"), all);
        Check(".cs struct tools.build.Log is unaffected",
            c.Records.Any(s => s.StartsWith($"struct|tools.build.Log|{cs}|", StringComparison.Ordinal)), all);

        var targets = WriteTargets(baseDir, "script-scope.targets", p1);
        var t = Canonicalize(ParseGraph(RunScanner(dir, "--targets", targets).Out));
        var fullP1 = c.Records.Where(s => IsDecl(s) && RecordPath(s) == p1).Select(s => s.Split('|')[1]).ToList();
        var targetedFqns = t.Records.Where(IsDecl).Select(s => s.Split('|')[1]).ToList();
        Check("targeted scan of tools/build.csx emits the same FQNs as the full scan",
            fullP1.Count > 0 && new SortedSet<string>(fullP1, StringComparer.Ordinal).SetEquals(targetedFqns),
            SetDiff(fullP1, targetedFqns));
    }

    // ------------------------------------------------------------------
    // ProjectFileReader: static .csproj reading (internal, reached by reflection)
    // ------------------------------------------------------------------

    private static object ReadProject(string csproj)
        => FrontendType("ProjectFileReader").GetMethod("Read")!.Invoke(null, new object[] { csproj })!;

    private static T Prop<T>(object project, string name) => (T)project.GetType().GetProperty(name)!.GetValue(project)!;

    /// <summary>
    /// SDK-style: default **/*.cs minus bin/obj and Compile Remove, plus Compile Include;
    /// PackageReference Version from attribute and child; FrameworkReference;
    /// first TargetFrameworks entry; DefineConstants (conditioned group ignored); LangVersion; ProjectReference.
    /// </summary>
    private static void TestProjectSdkStyle(string baseDir)
    {
        var root = Path.GetFullPath(Path.Combine(baseDir, "csproj-sdk"));
        var dir = Path.Combine(root, "App");
        var a = WriteSource(dir, "A.cs", "class A { }");
        var b = WriteSource(Path.Combine(dir, "sub"), "B.cs", "class B { }");
        var shared = WriteSource(Path.Combine(root, "Shared"), "S.cs", "class S { }");
        WriteSource(dir, "Gen.cs", "class Gen { }");
        WriteSource(Path.Combine(dir, "bin", "Debug"), "X.cs", "class X { }");
        WriteSource(Path.Combine(dir, "obj"), "Y.cs", "class Y { }");
        var csproj = WriteSource(dir, "App.csproj", """
            <Project Sdk="Microsoft.NET.Sdk">
              <PropertyGroup>
                <TargetFrameworks>net8.0;net6.0</TargetFrameworks>
                <LangVersion>12.0</LangVersion>
                <DefineConstants>FOO;$(DefineConstants);;BAR</DefineConstants>
              </PropertyGroup>
              <PropertyGroup Condition="'$(Configuration)' == 'Debug'">
                <DefineConstants>DEBUGONLY</DefineConstants>
              </PropertyGroup>
              <ItemGroup>
                <Compile Include="../Shared/S.cs" />
                <Compile Remove="Gen.cs" />
                <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
                <PackageReference Include="Serilog"><Version>3.1.0</Version></PackageReference>
                <FrameworkReference Include="Microsoft.AspNetCore.App" />
                <ProjectReference Include="..\Lib\Lib.csproj" />
              </ItemGroup>
            </Project>
            """);

        var p = ReadProject(csproj);
        Check("SDK-style csproj: Sdk read from the root attribute",
            Prop<string>(p, "Sdk") == "Microsoft.NET.Sdk" && Prop<bool>(p, "IsSdkStyle"), Prop<string>(p, "Sdk"));
        var sources = Prop<List<string>>(p, "SourcePaths");
        var want = new[] { a, b, shared };
        Check("SDK-style csproj: default glob + Include, minus Remove and bin/obj",
            new SortedSet<string>(want, StringComparer.Ordinal).SetEquals(sources), SetDiff(want, sources));
        var packages = Prop<List<(string Include, string? Version)>>(p, "PackageReferences");
        Check("SDK-style csproj: PackageReference Version from the attribute and from a Version child",
            packages.SequenceEqual(new (string, string?)[] { ("Newtonsoft.Json", "13.0.1"), ("Serilog", "3.1.0") }),
            Join(packages.Select(x => x.ToString())));
        var frameworks = Prop<List<string>>(p, "FrameworkReferences");
        Check("SDK-style csproj: FrameworkReference read",
            frameworks.SequenceEqual(new[] { "Microsoft.AspNetCore.App" }), Join(frameworks));
        Check("SDK-style csproj: first-listed TargetFrameworks entry selected",
            Prop<string>(p, "TargetFramework") == "net8.0", Prop<string>(p, "TargetFramework"));
        var symbols = Prop<List<string>>(p, "PreprocessorSymbols");
        Check("SDK-style csproj: DefineConstants split, $(…)/empties dropped, conditioned group ignored",
            symbols.SequenceEqual(new[] { "FOO", "BAR" }), Join(symbols));
        Check("SDK-style csproj: LangVersion read", Prop<string>(p, "LanguageVersion") == "12.0", Prop<string>(p, "LanguageVersion"));
        var refs = Prop<List<string>>(p, "ProjectReferences");
        Check("SDK-style csproj: ProjectReference kept as written",
            refs.SequenceEqual(new[] { @"..\Lib\Lib.csproj" }), Join(refs));
    }

    /// <summary>An &lt;Sdk Name&gt; element marks SDK style; EnableDefaultCompileItems=false disables globbing; TargetFramework beats TargetFrameworks.</summary>
    private static void TestProjectSdkElementNoDefaultItems(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "csproj-sdk-element"));
        var a = WriteSource(dir, "A.cs", "class A { }");
        WriteSource(dir, "B.cs", "class B { }");
        var csproj = WriteSource(dir, "Web.csproj", """
            <Project>
              <Sdk Name="Microsoft.NET.Sdk.Web" />
              <PropertyGroup>
                <TargetFrameworks>net8.0;net6.0</TargetFrameworks>
                <TargetFramework>net9.0</TargetFramework>
                <EnableDefaultCompileItems>false</EnableDefaultCompileItems>
              </PropertyGroup>
              <ItemGroup><Compile Include="A.cs" /></ItemGroup>
            </Project>
            """);

        var p = ReadProject(csproj);
        Check("<Sdk Name> element csproj is SDK-style",
            Prop<string>(p, "Sdk") == "Microsoft.NET.Sdk.Web" && Prop<bool>(p, "IsSdkStyle"), Prop<string>(p, "Sdk"));
        var sources = Prop<List<string>>(p, "SourcePaths");
        Check("EnableDefaultCompileItems=false compiles only the explicit Includes",
            sources.SequenceEqual(new[] { a }), Join(sources));
        Check("TargetFramework wins over TargetFrameworks",
            Prop<string>(p, "TargetFramework") == "net9.0", Prop<string>(p, "TargetFramework"));
    }

    /// <summary>.NET Framework style: explicit Compile items only, Reference HintPath as written, packages.config beside the csproj.</summary>
    private static void TestProjectFrameworkStyle(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "csproj-framework"));
        var program = WriteSource(dir, "Program.cs", "class P { }");
        var info = WriteSource(Path.Combine(dir, "Properties"), "AssemblyInfo.cs", "");
        WriteSource(dir, "Unlisted.cs", "class U { }");
        WriteSource(dir, "packages.config", """
            <?xml version="1.0" encoding="utf-8"?>
            <packages>
              <package id="Newtonsoft.Json" version="12.0.3" targetFramework="net472" />
            </packages>
            """);
        var csproj = WriteSource(dir, "Legacy.csproj", """
            <?xml version="1.0" encoding="utf-8"?>
            <Project ToolsVersion="15.0" xmlns="http://schemas.microsoft.com/developer/msbuild/2003">
              <PropertyGroup>
                <TargetFrameworkVersion>v4.7.2</TargetFrameworkVersion>
                <RootNamespace>Legacy</RootNamespace>
              </PropertyGroup>
              <ItemGroup>
                <Reference Include="System" />
                <Reference Include="Newtonsoft.Json">
                  <HintPath>..\packages\Newtonsoft.Json.12.0.3\lib\net45\Newtonsoft.Json.dll</HintPath>
                </Reference>
              </ItemGroup>
              <ItemGroup>
                <Compile Include="Program.cs" />
                <Compile Include="Properties\AssemblyInfo.cs" />
              </ItemGroup>
            </Project>
            """);

        var p = ReadProject(csproj);
        Check(".NET Framework-style csproj is not SDK-style",
            Prop<string>(p, "Sdk") == "" && !Prop<bool>(p, "IsSdkStyle"), Prop<string>(p, "Sdk"));
        var sources = Prop<List<string>>(p, "SourcePaths");
        Check(".NET Framework-style csproj compiles only the explicit Compile items",
            sources.SequenceEqual(new[] { program, info }), Join(sources));
        var assemblies = Prop<List<(string Include, string? HintPath)>>(p, "AssemblyReferences");
        Check(".NET Framework-style csproj: Reference HintPath kept as written",
            assemblies.SequenceEqual(new (string, string?)[]
            {
                ("System", null),
                ("Newtonsoft.Json", @"..\packages\Newtonsoft.Json.12.0.3\lib\net45\Newtonsoft.Json.dll"),
            }), Join(assemblies.Select(x => x.ToString())));
        var packages = Prop<List<(string Include, string? Version)>>(p, "PackageReferences");
        Check(".NET Framework-style csproj: packages.config packages read",
            packages.SequenceEqual(new (string, string?)[] { ("Newtonsoft.Json", "12.0.3") }),
            Join(packages.Select(x => x.ToString())));
        Check(".NET Framework-style csproj: RootNamespace read", Prop<string>(p, "RootNamespace") == "Legacy", Prop<string>(p, "RootNamespace"));
    }

    // ------------------------------------------------------------------
    // SolutionReader: static .sln reading (internal, reached by reflection)
    // ------------------------------------------------------------------

    private static List<string> ReadSolution(string sln)
        => (List<string>)FrontendType("SolutionReader").GetMethod("Read")!.Invoke(null, new object[] { sln })!;

    private const string SlnBody = """
        Microsoft Visual Studio Solution File, Format Version 12.00
        Project("{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}") = "App", "src\App\App.csproj", "{11111111-1111-1111-1111-111111111111}"
        EndProject
        Project("{2150E333-8FDC-42A3-9474-1A3956D46DE8}") = "Folder", "Folder", "{22222222-2222-2222-2222-222222222222}"
        EndProject
        Project("{F184B08F-C81C-45F6-A57F-5ABD9991F28F}") = "VbLib", "src\VbLib\VbLib.vbproj", "{33333333-3333-3333-3333-333333333333}"
        EndProject
        Project("{9A19103F-16F7-4668-BE54-9A1E7A4F7556}") = "Shared", "..\shared\Shared.csproj", "{44444444-4444-4444-4444-444444444444}"
        EndProject
        Project("{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}") = "AppAgain", "src\App\App.csproj", "{55555555-5555-5555-5555-555555555555}"
        EndProject
        """;

    /// <summary>
    /// Every C# Project entry yields its .csproj resolved against the .sln dir (nested backslash
    /// and ..\ paths); solution folders and .vbproj are skipped; a duplicate listing is de-duped in
    /// listed order — over an LF file and a CRLF + BOM file.
    /// </summary>
    private static void TestSolutionReader(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "sln", "root"));
        var want = new[]
        {
            Path.GetFullPath(Path.Combine(dir, "src", "App", "App.csproj")),
            Path.GetFullPath(Path.Combine(dir, "..", "shared", "Shared.csproj")),
        };
        var lf = WriteSource(dir, "Lf.sln", SlnBody.Replace("\r\n", "\n"));
        var got = ReadSolution(lf);
        Check("sln: csproj entries resolved vs the .sln dir, folders/.vbproj skipped, de-duped in order",
            got.SequenceEqual(want), Join(got));

        var crlf = Path.Combine(dir, "Crlf.sln");
        File.WriteAllText(crlf, SlnBody.Replace("\r\n", "\n").Replace("\n", "\r\n"), new UTF8Encoding(true));
        got = ReadSolution(crlf);
        Check("sln: CRLF + BOM file reads identically", got.SequenceEqual(want), Join(got));
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
