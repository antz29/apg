using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Text;
using System.Text.Json;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;

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
            // Harness-wide reference isolation: no test may read the developer's NuGet cache, dotnet or Reference Assemblies dirs.
            string iso = Path.GetFullPath(Path.Combine(baseDir, "isolation"));
            HarnessNugetPackages = Directory.CreateDirectory(Path.Combine(iso, "nuget")).FullName;
            HarnessDotnetRoot = Directory.CreateDirectory(Path.Combine(iso, "dotnet")).FullName;
            HarnessRefAsmRoot = Directory.CreateDirectory(Path.Combine(iso, "refasm")).FullName;
            ApplyHarnessDefaults();

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
            TestPerProjectScan(baseDir);
            TestZeroProjectTree(baseDir);
            TestFilteredSharedFileRegistration(baseDir);
            TestProjectWalkSkips(baseDir);
            TestSlnProjectScope(baseDir);
            TestProjectWalkModuleScope(baseDir);
            TestProjectLoadedOnce(baseDir);
            TestDiscoverFilesScope(baseDir);
            TestOverloadFailureKeepsBoundMethod(baseDir);
            TestRestoredReferencesBindInScan(baseDir);
            TestReferencePrecedence(baseDir);
            TestAssetsLongFormTarget(baseDir);
            TestPackageFolderSelection(baseDir);
            TestPackagesFolderAncestorScope(baseDir);
            TestPackageVersionRules(baseDir);
            TestUnreadableReferenceLocations(baseDir);
            TestFrameworkPackSelection(baseDir);
            TestFrameworkNet4x(baseDir);
            TestFrameworkCarriedFallback(baseDir);
            TestFrameworkPartialPacks(baseDir);
            TestEnvRootOverrides(baseDir);
            TestUnrestoredPackageTargets(baseDir);
            TestFrontendAssemblyIsolation(baseDir);
            TestCachedReferenceIsolation(baseDir);
            TestSharedReferenceCache(baseDir);
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

    // Empty isolation roots set by Main before any test; deleted with baseDir on exit.
    private static string HarnessNugetPackages = "";
    private static string HarnessDotnetRoot = "";
    private static string HarnessRefAsmRoot = "";

    /// Sets NUGET_PACKAGES and both ReferenceResolver root overrides to the harness
    /// defaults; a test that swaps any of them in calls this in its finally (never unset/null).
    private static void ApplyHarnessDefaults()
    {
        Environment.SetEnvironmentVariable("NUGET_PACKAGES", HarnessNugetPackages);
        var resolver = FrontendType("ReferenceResolver");
        var flags = BindingFlags.NonPublic | BindingFlags.Static;
        resolver.GetProperty("DotnetRootOverride", flags)!.SetValue(null, HarnessDotnetRoot);
        resolver.GetProperty("ReferenceAssembliesRootOverride", flags)!.SetValue(null, HarnessRefAsmRoot);
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
    // Per-project scan: real SDK-style .csproj trees through Scanner.Run
    // ------------------------------------------------------------------

    /// <summary>
    /// App references a multi-targeted Lib defining FEATURE; loose .cs files sit
    /// outside both projects. Cross-project calls resolve in full and targeted
    /// scans to the same canonical FQN; DefineConstants gates per project; Lib
    /// compiles once; projectless File parents are the first namespace or empty.
    /// </summary>
    private static void TestPerProjectScan(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "per-project"));
        WriteSource(Path.Combine(dir, "Lib"), "Lib.csproj", """
            <Project Sdk="Microsoft.NET.Sdk">
              <PropertyGroup>
                <TargetFrameworks>net8.0;net9.0</TargetFrameworks>
                <DefineConstants>FEATURE</DefineConstants>
              </PropertyGroup>
            </Project>
            """);
        var util = WriteSource(Path.Combine(dir, "Lib"), "Util.cs",
            "namespace Lib;\n\npublic class Util\n{\n    public static int Twice(int x) { return x * 2; }\n#if FEATURE\n    public void Gated() { }\n#endif\n}\n");
        WriteSource(Path.Combine(dir, "App"), "App.csproj", """
            <Project Sdk="Microsoft.NET.Sdk">
              <PropertyGroup><TargetFramework>net9.0</TargetFramework></PropertyGroup>
              <ItemGroup><ProjectReference Include="..\Lib\Lib.csproj" /></ItemGroup>
            </Project>
            """);
        var caller = WriteSource(Path.Combine(dir, "App"), "Caller.cs",
            "namespace App;\n\npublic class Caller\n{\n    public int Go() { return Lib.Util.Twice(1); }\n#if FEATURE\n    public void AppGated() { }\n#endif\n}\n");
        var loose = WriteSource(Path.Combine(dir, "Loose"), "Loose.cs",
            "namespace First.Inner { public class X { } }\nnamespace Second { public class Y { } }\n");
        var bare = WriteSource(Path.Combine(dir, "Loose"), "Bare.cs", "public class Bare { }\n");

        var r = RunScanner(dir);
        Check("per-project scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        var g = ParseGraph(r.Out);
        var c = Canonicalize(g);
        var all = Join(c.Records) + "\nSTDERR:\n" + r.Err;

        const string callee = "Lib.Util.Twice";
        Check("full scan: cross-project call is a resolved calls edge to Lib.Util.Twice",
            c.NodeFqns.Contains(callee) && c.Records.Contains($"calls|App.Caller.Go|{callee}"), all);

        var funcFqns = c.Records.Where(s => s.StartsWith("function|", StringComparison.Ordinal)).Select(s => s.Split('|')[1]).ToList();
        var structFqns = c.Records.Where(s => s.StartsWith("struct|", StringComparison.Ordinal)).Select(s => s.Split('|')[1]).ToList();
        Check("DefineConstants: FEATURE-gated declaration emitted in the defining project (Lib.Util.Gated)",
            funcFqns.Contains("Lib.Util.Gated"), all);
        Check("DefineConstants: FEATURE-gated declaration absent where undefined (App.Caller.AppGated)",
            !funcFqns.Any(f => f.Contains("AppGated", StringComparison.Ordinal)), all);
        Check("multi-targeted Lib compiles once: one Util struct and one Twice function record",
            g.Structs.Count(s => s.Parent == "Lib" && s.Name == "Util") == 1
            && g.Funcs.Count(f => f.Parent == "Lib.Util" && f.Name == "Twice") == 1, all);
        Check("no duplicate declaration records",
            funcFqns.Count == g.Funcs.Count && structFqns.Count == g.Structs.Count
            && g.Files.Select(f => f.Split('|')[0]).Distinct().Count() == g.Files.Count, all);

        Check("projectless .cs File parent is its first namespace's module identity (First.Inner)",
            g.Files.Any(f => f.StartsWith($"{loose}|First.Inner|", StringComparison.Ordinal)), Join(g.Files));
        Check("projectless .cs declarations are graphed", structFqns.Contains("First.Inner.X") && structFqns.Contains("Second.Y"), all);
        Check("namespace-less projectless .cs File record has an empty parent",
            g.Files.Any(f => f.StartsWith($"{bare}||", StringComparison.Ordinal)), Join(g.Files));

        // Targeted: the calling file only; Lib's file is not emitted.
        var targets = WriteTargets(baseDir, "per-project.targets", caller);
        var tr = RunScanner(dir, "--targets", targets);
        Check("targeted per-project scan exits 0", tr.Code == 0, "exit " + tr.Code + "\n" + tr.Err);
        var tg = ParseGraph(tr.Out);
        var tc = Canonicalize(tg);
        var tall = Join(tc.Records) + "\nSTDERR:\n" + tr.Err;
        Check("targeted scan omits the referenced project's declarations",
            !tc.Records.Any(s => IsDecl(s) && RecordPath(s) == util), tall);
        var goId = tg.Funcs.FirstOrDefault(f => f.Parent == "App.Caller" && f.Name == "Go")?.Id ?? "";
        Check("targeted scan: cross-project call lands on the callee's canonical FQN, identical to the full scan's",
            goId != "" && tg.Edges.Any(e => e.Type == "calls" && e.From == goId && e.To == callee), tall);
    }

    /// <summary>
    /// Projects A and B both compile Shared/S.cs (A owns it: first in build
    /// order). B's target file calls S.Hit through B's copy; a filtered scan
    /// must count S.Hit once (no overload suffix) and map B's symbol back to
    /// the owner's, landing on the full scan's plain FQN.
    /// </summary>
    private static void TestFilteredSharedFileRegistration(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "shared-file"));
        const string proj = """
            <Project Sdk="Microsoft.NET.Sdk">
              <PropertyGroup><TargetFramework>net9.0</TargetFramework></PropertyGroup>
              <ItemGroup><Compile Include="../Shared/S.cs" /></ItemGroup>
            </Project>
            """;
        WriteSource(Path.Combine(dir, "A"), "A.csproj", proj);
        WriteSource(Path.Combine(dir, "B"), "B.csproj", proj);
        WriteSource(Path.Combine(dir, "Shared"), "S.cs", "namespace Sh;\n\npublic class S\n{\n    public static void Hit() { }\n}\n");
        var caller = WriteSource(Path.Combine(dir, "B"), "Caller.cs",
            "namespace Bn;\n\npublic class Caller\n{\n    public void Go() { Sh.S.Hit(); }\n}\n");

        const string callee = "Sh.S.Hit";
        var full = RunScanner(dir);
        Check("shared-file full scan exits 0", full.Code == 0, "exit " + full.Code + "\n" + full.Err);
        var fg = ParseGraph(full.Out);
        var fc = Canonicalize(fg);
        var fall = Join(fc.Records) + "\nSTDERR:\n" + full.Err;
        Check("shared-file full scan emits Sh.S.Hit exactly once",
            fg.Funcs.Count(f => f.Parent == "Sh.S" && f.Name == "Hit") == 1 && fc.NodeFqns.Contains(callee), fall);
        Check("shared-file full scan: calls edge to the plain FQN Sh.S.Hit",
            fc.Records.Contains($"calls|Bn.Caller.Go|{callee}"), fall);

        var targets = WriteTargets(baseDir, "shared-file.targets", caller);
        var tr = RunScanner(dir, "--targets", targets);
        Check("shared-file targeted scan exits 0", tr.Code == 0, "exit " + tr.Code + "\n" + tr.Err);
        var tg = ParseGraph(tr.Out);
        var tall = Join(Canonicalize(tg).Records) + "\nSTDERR:\n" + tr.Err;
        var goId = tg.Funcs.FirstOrDefault(f => f.Parent == "Bn.Caller" && f.Name == "Go")?.Id ?? "";
        Check("shared-file targeted scan: call lands on the plain FQN Sh.S.Hit (no params suffix), as in the full scan",
            goId != "" && tg.Edges.Any(e => e.Type == "calls" && e.From == goId && e.To == callee), tall);
    }

    /// <summary>A tree with no .csproj/.sln is still fully graphed: files, declarations and cross-file calls.</summary>
    private static void TestZeroProjectTree(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "zero-project"));
        var a = WriteSource(dir, "A.cs", "namespace Z;\n\npublic class A { public void Run() { new B().Hit(); } }\n");
        var b = WriteSource(Path.Combine(dir, "sub"), "B.cs", "namespace Z;\n\npublic class B { public void Hit() { } }\n");

        var r = RunScanner(dir);
        Check("zero-project scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        var c = Canonicalize(ParseGraph(r.Out));
        var all = Join(c.Records) + "\nSTDERR:\n" + r.Err;
        Check("zero-project tree: every file has a File record under its namespace",
            c.Records.Any(s => s.StartsWith($"file|{a}|Z|", StringComparison.Ordinal))
            && c.Records.Any(s => s.StartsWith($"file|{b}|Z|", StringComparison.Ordinal)), all);
        Check("zero-project tree: declarations graphed", c.NodeFqns.Contains("Z.A.Run") && c.NodeFqns.Contains("Z.B.Hit"), all);
        Check("zero-project tree: cross-file call resolved", c.Records.Contains("calls|Z.A.Run|Z.B.Hit"), all);
    }

    // ------------------------------------------------------------------
    // Project-discovery scope: a project is observed as loaded only through a
    // project-dependent effect — its DefineConstants symbol ungating a
    // declaration in a file it compiles.
    // ------------------------------------------------------------------

    /// SDK-style csproj with no default globbing: defines `symbol`, compiles exactly `includes`.
    private static string ScopeProject(string symbol, params string[] includes) =>
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net9.0</TargetFramework>\n"
        + $"    <DefineConstants>{symbol}</DefineConstants>\n    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>\n  </PropertyGroup>\n  <ItemGroup>"
        + string.Concat(includes.Select(i => $"<Compile Include=\"{i}\" />")) + "</ItemGroup>\n</Project>\n";

    private static string Gated(string symbol, string method) => $"#if {symbol}\n    public void {method}() {{ }}\n#endif\n";

    /// <summary>
    /// A .csproj under bin/, obj/, node_modules/ or a dot-dir, or matching an
    /// exclude pattern (case-insensitively), is never loaded: none of their
    /// symbols ungates Src/X.cs, which only they compile; a sibling project is loaded.
    /// </summary>
    private static void TestProjectWalkSkips(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "project-skips"));
        var skipped = new (string Dir, string Symbol, string Method)[]
        {
            ("bin", "FROM_BIN", "FromBin"), ("obj", "FROM_OBJ", "FromObj"),
            ("node_modules", "FROM_NM", "FromNodeModules"), (".hidden", "FROM_DOT", "FromDotDir"),
            ("SkipMe", "FROM_EXCL", "FromExcluded"),
        };
        foreach (var s in skipped) WriteSource(Path.Combine(dir, s.Dir), "P.csproj", ScopeProject(s.Symbol, "../Src/X.cs"));
        WriteSource(Path.Combine(dir, "Src"), "X.cs",
            "namespace Sk;\n\npublic class X\n{\n" + string.Concat(skipped.Select(s => Gated(s.Symbol, s.Method))) + "}\n");
        WriteSource(Path.Combine(dir, "Good"), "Good.csproj", ScopeProject("GOOD", "G.cs"));
        WriteSource(Path.Combine(dir, "Good"), "G.cs", "namespace Sk;\n\npublic class G\n{\n" + Gated("GOOD", "GoodGated") + "}\n");

        var r = RunScanner(dir, "skipme");
        Check("project-skip scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        var g = ParseGraph(r.Out);
        var all = Join(Canonicalize(g).Records) + "\nSTDERR:\n" + r.Err;
        Check("project walk: a sibling project is loaded (its DefineConstants ungates Sk.G.GoodGated)",
            g.Funcs.Any(f => f.Parent == "Sk.G" && f.Name == "GoodGated"), all);
        Check("project walk: Src/X.cs is still graphed (projectless)", g.Structs.Any(s => s.Parent == "Sk" && s.Name == "X"), all);
        foreach (var s in skipped)
        {
            Check($"project walk: a .csproj under {s.Dir}/ is not loaded ({s.Method} stays gated)",
                !g.Funcs.Any(f => f.Name == s.Method), all);
        }
    }

    /// <summary>
    /// A .sln-listed project is held to the walk's scope: one under a dot-dir,
    /// bin/ or obj/ ancestor, or outside the scan root, is not loaded, so the
    /// shared Src source it compiles stays projectless (its symbol never ungates).
    /// </summary>
    private static void TestSlnProjectScope(string baseDir)
    {
        var outer = Path.GetFullPath(Path.Combine(baseDir, "sln-scope"));
        var dir = Path.Combine(outer, "root");
        var listed = new (string Dir, string Rel, string Symbol, string Method)[]
        {
            (Path.Combine(dir, ".build", "Gen"), @".build\Gen\Gen.csproj", "FROM_DOT", "FromDotDir"),
            (Path.Combine(dir, "bin", "B"), @"bin\B\Gen.csproj", "FROM_BIN", "FromBin"),
            (Path.Combine(dir, "obj", "O"), @"obj\O\Gen.csproj", "FROM_OBJ", "FromObj"),
            (Path.Combine(outer, "Other", "x"), @"..\Other\x\Gen.csproj", "FROM_OUT", "FromOutside"),
        };
        // One shared source per project, so build order never masks a case.
        foreach (var s in listed)
        {
            WriteSource(s.Dir, "Gen.csproj", ScopeProject(s.Symbol, (s.Dir.StartsWith(dir) ? "../../Src/" : "../../root/Src/") + s.Method + ".cs"));
            WriteSource(Path.Combine(dir, "Src"), s.Method + ".cs", $"namespace Sl;\n\npublic class {s.Method}\n{{\n" + Gated(s.Symbol, "Gated") + "}\n");
        }
        WriteSource(dir, "App.sln", "Microsoft Visual Studio Solution File, Format Version 12.00\n" + string.Concat(listed.Select((s, i) =>
            $"Project(\"{{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}}\") = \"G{i}\", \"{s.Rel}\", \"{{1111111{i}-1111-1111-1111-111111111111}}\"\nEndProject\n")));

        var r = RunScanner(dir);
        Check("sln-scope scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        var g = ParseGraph(r.Out);
        var all = Join(Canonicalize(g).Records) + "\nSTDERR:\n" + r.Err;
        foreach (var s in listed)
        {
            Check($"sln scope: .sln-listed {s.Rel} is not loaded (Src/{s.Method}.cs graphed projectless, Sl.{s.Method}.Gated stays gated)",
                g.Structs.Any(x => x.Parent == "Sl" && x.Name == s.Method) && !g.Funcs.Any(f => f.Parent == "Sl." + s.Method && f.Name == "Gated"), all);
        }
    }

    /// <summary>
    /// --module src/A: a project in the module dir (below the non-module dir
    /// src) is loaded; a project outside the module dirs and the sibling
    /// src/AB (segment boundary) are not, and src/AB's sources are not scanned.
    /// </summary>
    private static void TestProjectWalkModuleScope(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "project-module"));
        var a = Path.Combine(dir, "src", "A");
        WriteSource(a, "A.csproj", ScopeProject("AMOD", "A.cs"));
        WriteSource(a, "A.cs", "namespace Ma;\n\npublic class A\n{\n" + Gated("AMOD", "InA") + "}\n");
        WriteSource(a, "Extra.cs", "namespace Ma;\n\npublic class Extra\n{\n" + Gated("ABMOD", "FromAB") + Gated("OUTSIDE", "FromOutside") + "}\n");
        WriteSource(Path.Combine(dir, "src", "AB"), "AB.csproj", ScopeProject("ABMOD", "../A/Extra.cs", "AB.cs"));
        WriteSource(Path.Combine(dir, "src", "AB"), "AB.cs", "namespace Mab;\n\npublic class AB { }\n");
        WriteSource(Path.Combine(dir, "outside"), "O.csproj", ScopeProject("OUTSIDE", "../src/A/Extra.cs"));
        WriteSource(dir, "Root.cs", "namespace Mr;\n\npublic class Root { }\n");

        var r = RunScanner(dir, "--module", "src/A");
        Check("module-scoped project scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        var g = ParseGraph(r.Out);
        var all = Join(Canonicalize(g).Records) + "\nSTDERR:\n" + r.Err;
        Check("--module: a project in a module dir nested below a non-module dir is loaded (Ma.A.InA)",
            g.Funcs.Any(f => f.Parent == "Ma.A" && f.Name == "InA"), all);
        Check("--module: a project outside the module dirs is not loaded (FromOutside stays gated)",
            !g.Funcs.Any(f => f.Name == "FromOutside"), all);
        Check("--module src/A never admits the sibling project src/AB (FromAB stays gated)",
            !g.Funcs.Any(f => f.Name == "FromAB"), all);
        Check("--module src/A: src/AB and root sources are not scanned; Extra.cs is (projectless)",
            !g.Structs.Any(s => s.Name is "AB" or "Root") && g.Structs.Any(s => s.Parent == "Ma" && s.Name == "Extra"), all);
    }

    /// <summary>A .csproj reached by the walk and through two .sln files is loaded exactly once.</summary>
    private static void TestProjectLoadedOnce(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "project-once"));
        WriteSource(Path.Combine(dir, "P"), "P.csproj", ScopeProject("ONCE", "P.cs"));
        var pcs = WriteSource(Path.Combine(dir, "P"), "P.cs", "namespace On;\n\npublic class P\n{\n" + Gated("ONCE", "Gated") + "}\n");
        const string entry = "Microsoft Visual Studio Solution File, Format Version 12.00\n"
            + "Project(\"{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}\") = \"P\", \"PATH\", \"{11111111-1111-1111-1111-111111111111}\"\nEndProject\n";
        WriteSource(dir, "a.sln", entry.Replace("PATH", @"P\P.csproj"));
        WriteSource(Path.Combine(dir, "sub"), "b.sln", entry.Replace("PATH", @"..\P\P.csproj"));

        var r = RunScanner(dir);
        Check("loaded-once scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        var g = ParseGraph(r.Out);
        var all = Join(Canonicalize(g).Records) + "\nSTDERR:\n" + r.Err;
        Check("walk + two .sln: the project is loaded (On.P.Gated ungated) exactly once",
            g.Funcs.Count(f => f.Parent == "On.P" && f.Name == "Gated") == 1, all);
        Check("walk + two .sln: no duplicate struct or File records",
            g.Structs.Count(s => s.Parent == "On" && s.Name == "P") == 1
            && g.Files.Count(f => f.StartsWith(pcs + "|", StringComparison.Ordinal)) == 1, all);
    }

    /// <summary>
    /// DiscoverFiles after the IsExcluded refactor: dot-dir/bin/obj/node_modules
    /// skipped, exclude patterns case-insensitive, and --module matched on a
    /// segment boundary (src/AB is not inside module src/A).
    /// </summary>
    private static void TestDiscoverFilesScope(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "discover-scope"));
        var a = Path.Combine(dir, "src", "A");
        var inA = WriteSource(a, "a.cs", "class A { }");
        var inSub = WriteSource(Path.Combine(a, "sub"), "s.cs", "class S { }");
        foreach (var skip in new[] { "bin", "obj", "node_modules", ".git" }) WriteSource(Path.Combine(a, skip), "x.cs", "class X { }");
        var ab = WriteSource(Path.Combine(dir, "src", "AB"), "ab.cs", "class AB { }");
        WriteSource(Path.Combine(dir, "other"), "o.cs", "class O { }");
        var top = WriteSource(dir, "Top.cs", "class T { }");

        SortedSet<string> Discover(List<string> modules, List<string> excludes)
        {
            var discovery = Activator.CreateInstance(FrontendType("SourceDiscovery"), modules, excludes)!;
            var found = (IEnumerable<string>)discovery.GetType().GetMethod("DiscoverFiles")!.Invoke(discovery, new object[] { dir })!;
            return new SortedSet<string>(found, StringComparer.Ordinal);
        }

        var want = new SortedSet<string>(StringComparer.Ordinal) { inA, inSub, ab, top };
        var got = Discover(new List<string>(), new List<string> { "OTHER" });
        Check("DiscoverFiles: dot-dir/bin/obj/node_modules skipped, exclude pattern matched case-insensitively",
            want.SetEquals(got), SetDiff(want, got));

        want = new SortedSet<string>(StringComparer.Ordinal) { inA, inSub };
        got = Discover(new List<string> { a }, new List<string>());
        Check("DiscoverFiles: --module src/A yields only files under src/A (sibling src/AB excluded)",
            want.SetEquals(got), SetDiff(want, got));
    }

    /// <summary>
    /// An invocation failing overload resolution against a bound stdlib method
    /// (Console.WriteLine(1, 2): no overload takes an int format) keeps the
    /// candidate's bound naming System.Console.WriteLine and its stdlib category,
    /// never the unbound as-written "Console.WriteLine"/unknown path.
    /// </summary>
    private static void TestOverloadFailureKeepsBoundMethod(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "overload-failure"));
        WriteSource(dir, "O.cs", "using System;\n\nnamespace Ov;\n\npublic class O { public void Run() { Console.WriteLine(1, 2); } }\n");

        // Bind against the carried runtime alone: fresh empty NuGet/dotnet/Reference Assemblies roots.
        var resolver = FrontendType("ReferenceResolver");
        var flags = BindingFlags.NonPublic | BindingFlags.Static;
        (int Code, string Out, string Err) r;
        try
        {
            Environment.SetEnvironmentVariable("NUGET_PACKAGES", Directory.CreateDirectory(Path.Combine(dir, "iso-nuget")).FullName);
            resolver.GetProperty("DotnetRootOverride", flags)!.SetValue(null, Directory.CreateDirectory(Path.Combine(dir, "iso-dotnet")).FullName);
            resolver.GetProperty("ReferenceAssembliesRootOverride", flags)!.SetValue(null, Directory.CreateDirectory(Path.Combine(dir, "iso-refasm")).FullName);
            r = RunScanner(dir);
        }
        finally
        {
            ApplyHarnessDefaults();
        }

        Check("overload-failure scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        var c = Canonicalize(ParseGraph(r.Out));
        var all = Join(c.Records) + "\nSTDERR:\n" + r.Err;
        var calls = c.Records.Where(s => s.StartsWith("unresolved_call|Ov.O.Run|", StringComparison.Ordinal)).ToList();
        Check("overload failure: exactly one unresolved_call, named by the bound candidate System.Console.WriteLine",
            calls.Count == 1 && calls[0] == "unresolved_call|Ov.O.Run|System.Console.WriteLine", all);
        Check("overload failure: the target is categorized stdlib from the bound containing type",
            c.Records.Contains("unresolved|System.Console.WriteLine|stdlib"), all);
        Check("overload failure: no unbound as-written target is emitted",
            !c.UnresFqns.Contains("Console.WriteLine") && !c.Records.Any(s => s.EndsWith("|unknown", StringComparison.Ordinal)), all);
    }

    // ------------------------------------------------------------------
    // ReferenceResolver: restored package/assembly references over temp trees,
    // with stub assemblies emitted in-process by Roslyn. Every case runs under
    // Isolated (its own NUGET_PACKAGES and root overrides, defaults restored after).
    // ------------------------------------------------------------------

    /// A stub assembly declaring `{ns}.T` with a static Hit(), compiled against the carried runtime.
    private static byte[] StubAssembly(string name, string ns)
    {
        var refs = (IEnumerable<MetadataReference>)FrontendType("CompilationLoader")
            .GetMethod("GetDefaultMetadataReferences", BindingFlags.NonPublic | BindingFlags.Static)!.Invoke(null, null)!;
        var compilation = CSharpCompilation.Create(name,
            new[] { CSharpSyntaxTree.ParseText($"namespace {ns} {{ public class T {{ public static void Hit() {{ }} }} }}") },
            refs, new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary));
        using var ms = new MemoryStream();
        var result = compilation.Emit(ms);
        if (!result.Success) throw new InvalidOperationException("stub emit failed:\n" + Join(result.Diagnostics.Select(d => d.ToString())));
        return ms.ToArray();
    }

    private static string PutStub(string path, byte[] image)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllBytes(path, image);
        return Path.GetFullPath(path);
    }

    /// SDK-style net8.0 App.csproj in `dir` with the given reference items.
    private static string RefProject(string dir, string items) => WriteSource(dir, "App.csproj",
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup>\n  <ItemGroup>"
        + items + "</ItemGroup>\n</Project>\n");

    private static string PkgRef(string id, string? version) => version == null
        ? $"<PackageReference Include=\"{id}\" />"
        : $"<PackageReference Include=\"{id}\" Version=\"{version}\" />";

    /// obj/project.assets.json under `projectDir`: one package under `targetKey`, its library path id/version, one package folder.
    private static void WriteAssets(string projectDir, string id, string version, string targetKey, string folder, string compile)
    {
        var key = $"{id}/{version}";
        var assets = new Dictionary<string, object>
        {
            ["version"] = 3,
            ["targets"] = new Dictionary<string, object>
            {
                [targetKey] = new Dictionary<string, object>
                {
                    [key] = new Dictionary<string, object> { ["type"] = "package", ["compile"] = new Dictionary<string, object> { [compile] = new Dictionary<string, object>() } },
                },
            },
            ["libraries"] = new Dictionary<string, object> { [key] = new Dictionary<string, object> { ["type"] = "package", ["path"] = $"{id.ToLowerInvariant()}/{version}" } },
            ["packageFolders"] = new Dictionary<string, object> { [folder + "/"] = new Dictionary<string, object>() },
        };
        WriteSource(Path.Combine(projectDir, "obj"), "project.assets.json", JsonSerializer.Serialize(assets));
    }

    /// Runs `body` with NUGET_PACKAGES = `nuget` and both ReferenceResolver root overrides on fresh empty
    /// dirs under `dir`; restores the harness defaults (never unset) in a finally.
    private static T Isolated<T>(string dir, string nuget, Func<T> body)
    {
        var resolver = FrontendType("ReferenceResolver");
        var flags = BindingFlags.NonPublic | BindingFlags.Static;
        try
        {
            Environment.SetEnvironmentVariable("NUGET_PACKAGES", Directory.CreateDirectory(nuget).FullName);
            resolver.GetProperty("DotnetRootOverride", flags)!.SetValue(null, Directory.CreateDirectory(Path.Combine(dir, "iso-dotnet")).FullName);
            resolver.GetProperty("ReferenceAssembliesRootOverride", flags)!.SetValue(null, Directory.CreateDirectory(Path.Combine(dir, "iso-refasm")).FullName);
            return body();
        }
        finally
        {
            ApplyHarnessDefaults();
        }
    }

    /// ReferenceResolver.Resolve over the read project, as full dll paths in result order.
    private static List<string> ResolvePaths(string csproj, string scanRoot)
        => ((IEnumerable<MetadataReference>)FrontendType("ReferenceResolver").GetMethod("Resolve")!.Invoke(null, new[] { ReadProject(csproj), scanRoot })!)
            .Select(r => Path.GetFullPath(((PortableExecutableReference)r).FilePath!)).ToList();

    /// <summary>
    /// A package from obj/project.assets.json, an assembly from its HintPath, a package from the
    /// solution packages/ folder and one from the global packages folder each bind the caller's
    /// call and type use into the stub's metadata (external), where an unrestored one stays unknown.
    /// </summary>
    private static void TestRestoredReferencesBindInScan(string baseDir)
    {
        var root = Path.GetFullPath(Path.Combine(baseDir, "refs-scan"));
        void Case(string name, string ns, Func<string, string, string> arrange)
        {
            var dir = Path.Combine(root, name);
            var app = Path.Combine(dir, "App");
            var nuget = Path.Combine(dir, "nuget");
            WriteSource(app, "Caller.cs", $"namespace App;\n\npublic class Caller\n{{\n    public {ns}.T Field;\n    public void Go() {{ {ns}.T.Hit(); }}\n}}\n");
            RefProject(app, arrange(dir, nuget));
            var r = Isolated(dir, nuget, () => RunScanner(dir));
            var c = Canonicalize(ParseGraph(r.Out));
            var all = Join(c.Records) + "\nSTDERR:\n" + r.Err;
            var category = name == "unrestored" ? "unknown" : "external";
            Check($"restored refs ({name}): scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
            Check($"restored refs ({name}): call {ns}.T.Hit is {category}",
                c.Records.Contains($"unresolved_call|App.Caller.Go|{ns}.T.Hit") && c.Records.Contains($"unresolved|{ns}.T.Hit|{category}"), all);
            Check($"restored refs ({name}): type use {ns}.T is {category}",
                c.Records.Contains($"unresolved|{ns}.T|{category}") && c.Records.Any(s => s.StartsWith("unresolved_use|", StringComparison.Ordinal) && s.EndsWith($"|{ns}.T", StringComparison.Ordinal)), all);
        }

        Case("assets", "StubAssets", (dir, _) =>
        {
            var folder = Path.Combine(dir, "restored");
            PutStub(Path.Combine(folder, "stub.assets", "1.0.0", "lib", "net8.0", "Stub.Assets.dll"), StubAssembly("Stub.Assets", "StubAssets"));
            WriteAssets(Path.Combine(dir, "App"), "Stub.Assets", "1.0.0", "net8.0", folder, "lib/net8.0/Stub.Assets.dll");
            return PkgRef("Stub.Assets", "1.0.0");
        });
        Case("hintpath", "StubHint", (dir, _) =>
        {
            PutStub(Path.Combine(dir, "lib", "Stub.Hint.dll"), StubAssembly("Stub.Hint", "StubHint"));
            return "<Reference Include=\"Stub.Hint\"><HintPath>..\\lib\\Stub.Hint.dll</HintPath></Reference>";
        });
        Case("packages-folder", "StubPkgs", (dir, _) =>
        {
            PutStub(Path.Combine(dir, "packages", "Stub.Pkgs.1.0.0", "lib", "net8.0", "Stub.Pkgs.dll"), StubAssembly("Stub.Pkgs", "StubPkgs"));
            return PkgRef("Stub.Pkgs", "1.0.0");
        });
        Case("global-folder", "StubGlobal", (_, nuget) =>
        {
            PutStub(Path.Combine(nuget, "stub.global", "1.0.0", "lib", "net8.0", "Stub.Global.dll"), StubAssembly("Stub.Global", "StubGlobal"));
            return PkgRef("Stub.Global", "1.0.0");
        });
        Case("unrestored", "StubMissing", (_, _) => PkgRef("Stub.Missing", "1.0.0"));
    }

    /// <summary>
    /// A package present in the assets file's folder, the packages/ folder and the global folder
    /// resolves from the assets file; without the assets file from packages/; with neither, from
    /// the global folder.
    /// </summary>
    private static void TestReferencePrecedence(string baseDir)
    {
        var root = Path.GetFullPath(Path.Combine(baseDir, "refs-precedence"));
        var image = StubAssembly("Stub.Prec", "StubPrec");
        string Assets(string dir) => PutStub(Path.Combine(dir, "restored", "stub.prec", "1.0.0", "lib", "net8.0", "Stub.Prec.dll"), image);
        string Packages(string dir) => PutStub(Path.Combine(dir, "packages", "Stub.Prec.1.0.0", "lib", "net8.0", "Stub.Prec.dll"), image);
        string Global(string dir) => PutStub(Path.Combine(dir, "nuget", "stub.prec", "1.0.0", "lib", "net8.0", "Stub.Prec.dll"), image);

        foreach (var (name, present) in new[] { ("assets", 3), ("packages", 2), ("global", 1) })
        {
            var dir = Path.Combine(root, name);
            var app = Path.Combine(dir, "App");
            var csproj = RefProject(app, PkgRef("Stub.Prec", "1.0.0"));
            var global = Global(dir);
            var packages = present >= 2 ? Packages(dir) : null;
            string? assets = null;
            if (present == 3)
            {
                assets = Assets(dir);
                WriteAssets(app, "Stub.Prec", "1.0.0", "net8.0", Path.Combine(dir, "restored"), "lib/net8.0/Stub.Prec.dll");
            }
            var want = assets ?? packages ?? global;
            var got = Isolated(dir, Path.Combine(dir, "nuget"), () => ResolvePaths(csproj, dir));
            Check($"precedence: with {present} location(s) the package resolves from {name} alone",
                got.SequenceEqual(new[] { want }), "want " + want + "\ngot:\n" + Join(got));
        }
    }

    /// <summary>
    /// An assets file keyed by the long form (.NETCoreApp,Version=v8.0) resolves like the short
    /// form (net8.0), the dll taken from its recorded packageFolders (not the global folder).
    /// </summary>
    private static void TestAssetsLongFormTarget(string baseDir)
    {
        var root = Path.GetFullPath(Path.Combine(baseDir, "refs-assets-form"));
        var image = StubAssembly("Stub.Form", "StubForm");
        var got = new Dictionary<string, (string Want, List<string> Got)>();
        foreach (var (name, key) in new[] { ("long", ".NETCoreApp,Version=v8.0"), ("short", "net8.0") })
        {
            var dir = Path.Combine(root, name);
            var app = Path.Combine(dir, "App");
            var csproj = RefProject(app, PkgRef("Stub.Form", "1.0.0"));
            var folder = Path.Combine(dir, "recorded-folder");
            var want = PutStub(Path.Combine(folder, "stub.form", "1.0.0", "lib", "net8.0", "Stub.Form.dll"), image);
            WriteAssets(app, "Stub.Form", "1.0.0", key, folder, "lib/net8.0/Stub.Form.dll");
            got[name] = (want, Isolated(dir, Path.Combine(dir, "nuget-empty"), () => ResolvePaths(csproj, dir)));
        }
        Check("assets long-form target key resolves the package from its recorded packageFolders",
            got["long"].Got.SequenceEqual(new[] { got["long"].Want }), Join(got["long"].Got));
        Check("assets short-form target key resolves the same way",
            got["short"].Got.SequenceEqual(new[] { got["short"].Want }), Join(got["short"].Got));
    }

    /// <summary>
    /// A package holding ref/ and lib/ resolves ref/ only; a net8.0 project picks net6.0 alone from
    /// net6.0/netstandard2.0/net48, and netstandard2.0 alone from netstandard2.0/net48.
    /// </summary>
    private static void TestPackageFolderSelection(string baseDir)
    {
        var root = Path.GetFullPath(Path.Combine(baseDir, "refs-folders"));
        var image = StubAssembly("Stub.Sel", "StubSel");
        var cases = new (string Name, string[] Folders, string Want)[]
        {
            ("ref-over-lib", new[] { "ref/net8.0", "lib/net8.0" }, "ref/net8.0"),
            ("net6-of-three", new[] { "lib/net6.0", "lib/netstandard2.0", "lib/net48" }, "lib/net6.0"),
            ("netstandard-of-two", new[] { "lib/netstandard2.0", "lib/net48" }, "lib/netstandard2.0"),
        };
        foreach (var (name, folders, wantFolder) in cases)
        {
            var dir = Path.Combine(root, name);
            var nuget = Path.Combine(dir, "nuget");
            var csproj = RefProject(Path.Combine(dir, "App"), PkgRef("Stub.Sel", "1.0.0"));
            var pkg = Path.Combine(nuget, "stub.sel", "1.0.0");
            foreach (var f in folders) PutStub(Path.Combine(pkg, f, "Stub.Sel.dll"), image);
            var want = Path.GetFullPath(Path.Combine(pkg, wantFolder, "Stub.Sel.dll"));
            var got = Isolated(dir, nuget, () => ResolvePaths(csproj, dir));
            Check($"package folder selection ({name}): exactly {wantFolder}", got.SequenceEqual(new[] { want }), "want " + want + "\ngot:\n" + Join(got));
        }
    }

    /// <summary>
    /// packages/ two levels above the project (below the scan root) is found; a packages/ above
    /// the scan root is ignored (global folder empty, so nothing resolves).
    /// </summary>
    private static void TestPackagesFolderAncestorScope(string baseDir)
    {
        var root = Path.GetFullPath(Path.Combine(baseDir, "refs-packages-scope"));
        var image = StubAssembly("Stub.Anc", "StubAnc");

        var below = Path.Combine(root, "below", "scan");
        var csproj = RefProject(Path.Combine(below, "x", "y", "App"), PkgRef("Stub.Anc", "1.0.0"));
        // Project dir x/y/App: x is two levels above it, the scan root three.
        var want = PutStub(Path.Combine(below, "x", "packages", "Stub.Anc.1.0.0", "lib", "net8.0", "Stub.Anc.dll"), image);
        var got = Isolated(below, Path.Combine(root, "below", "nuget-empty"), () => ResolvePaths(csproj, below));
        Check("packages/ two levels above the project, below the scan root, is used",
            got.SequenceEqual(new[] { want }), "want " + want + "\ngot:\n" + Join(got));

        var above = Path.Combine(root, "above");
        var scan = Path.Combine(above, "scan");
        csproj = RefProject(Path.Combine(scan, "App"), PkgRef("Stub.Anc", "1.0.0"));
        PutStub(Path.Combine(above, "packages", "Stub.Anc.1.0.0", "lib", "net8.0", "Stub.Anc.dll"), image);
        got = Isolated(scan, Path.Combine(above, "nuget-empty"), () => ResolvePaths(csproj, scan));
        Check("packages/ above the scan root is ignored", got.Count == 0, Join(got));
    }

    /// <summary>
    /// A version-less PackageReference takes its version from an ancestor Directory.Packages.props
    /// PackageVersion; a floating 1.* picks the highest present 1.x folder (1.10.0 over 1.2.0, not 2.0.0).
    /// </summary>
    private static void TestPackageVersionRules(string baseDir)
    {
        var root = Path.GetFullPath(Path.Combine(baseDir, "refs-versions"));
        var image = StubAssembly("Stub.Ver", "StubVer");

        var cpm = Path.Combine(root, "central");
        var nuget = Path.Combine(cpm, "nuget");
        WriteSource(cpm, "Directory.Packages.props",
            "<Project>\n  <ItemGroup><PackageVersion Include=\"Stub.Ver\" Version=\"2.0.0\" /></ItemGroup>\n</Project>\n");
        var csproj = RefProject(Path.Combine(cpm, "src", "App"), PkgRef("Stub.Ver", null));
        foreach (var v in new[] { "1.0.0", "2.0.0", "3.0.0" }) PutStub(Path.Combine(nuget, "stub.ver", v, "lib", "net8.0", "Stub.Ver.dll"), image);
        var want = Path.GetFullPath(Path.Combine(nuget, "stub.ver", "2.0.0", "lib", "net8.0", "Stub.Ver.dll"));
        var got = Isolated(cpm, nuget, () => ResolvePaths(csproj, cpm));
        Check("version-less PackageReference takes the ancestor Directory.Packages.props PackageVersion",
            got.SequenceEqual(new[] { want }), "want " + want + "\ngot:\n" + Join(got));

        var flt = Path.Combine(root, "floating");
        nuget = Path.Combine(flt, "nuget");
        csproj = RefProject(Path.Combine(flt, "App"), PkgRef("Stub.Ver", "1.*"));
        foreach (var v in new[] { "1.2.0", "1.10.0", "2.0.0" }) PutStub(Path.Combine(nuget, "stub.ver", v, "lib", "net8.0", "Stub.Ver.dll"), image);
        want = Path.GetFullPath(Path.Combine(nuget, "stub.ver", "1.10.0", "lib", "net8.0", "Stub.Ver.dll"));
        got = Isolated(flt, nuget, () => ResolvePaths(csproj, flt));
        Check("floating 1.* picks the highest present 1.x folder", got.SequenceEqual(new[] { want }), "want " + want + "\ngot:\n" + Join(got));
    }

    /// <summary>
    /// An unreadable obj/project.assets.json and an unreadable packages/ folder count as not found:
    /// the scan exits 0 and the package resolves from the global folder, or stays unknown without one.
    /// Skipped where permissions are not enforced (Windows, root).
    /// </summary>
    private static void TestUnreadableReferenceLocations(string baseDir)
    {
        if (OperatingSystem.IsWindows()) { Console.WriteLine("SKIP: unreadable reference locations (Windows)"); return; }
        var root = Path.GetFullPath(Path.Combine(baseDir, "refs-unreadable"));
        var image = StubAssembly("Stub.Lock", "StubLock");
        foreach (var withGlobal in new[] { true, false })
        {
            var name = withGlobal ? "global" : "none";
            var dir = Path.Combine(root, name);
            var app = Path.Combine(dir, "App");
            var nuget = Path.Combine(dir, "nuget");
            WriteSource(app, "Caller.cs", "namespace App;\n\npublic class Caller\n{\n    public void Go() { StubLock.T.Hit(); }\n}\n");
            var csproj = RefProject(app, PkgRef("Stub.Lock", "1.0.0"));
            PutStub(Path.Combine(dir, "restored", "stub.lock", "1.0.0", "lib", "net8.0", "Stub.Lock.dll"), image);
            WriteAssets(app, "Stub.Lock", "1.0.0", "net8.0", Path.Combine(dir, "restored"), "lib/net8.0/Stub.Lock.dll");
            PutStub(Path.Combine(dir, "packages", "Stub.Lock.1.0.0", "lib", "net8.0", "Stub.Lock.dll"), image);
            var global = withGlobal ? PutStub(Path.Combine(nuget, "stub.lock", "1.0.0", "lib", "net8.0", "Stub.Lock.dll"), image) : null;
            var assets = Path.Combine(app, "obj", "project.assets.json");
            var packages = Path.Combine(dir, "packages");
            var fileMode = File.GetUnixFileMode(assets);
            var dirMode = File.GetUnixFileMode(packages);
            try
            {
                File.SetUnixFileMode(assets, UnixFileMode.None);
                File.SetUnixFileMode(packages, UnixFileMode.None);
                try
                {
                    File.ReadAllBytes(assets);
                    Console.WriteLine("SKIP: unreadable reference locations (permissions not enforced)");
                    return;
                }
                catch (UnauthorizedAccessException) { }
                var got = Isolated(dir, nuget, () => ResolvePaths(csproj, dir));
                var want = global == null ? Array.Empty<string>() : new[] { global };
                Check($"unreadable locations ({name}): resolve falls through", got.SequenceEqual(want), "want " + Join(want) + "\ngot:\n" + Join(got));
                var r = Isolated(dir, nuget, () => RunScanner(dir));
                var c = Canonicalize(ParseGraph(r.Out));
                var category = withGlobal ? "external" : "unknown";
                Check($"unreadable locations ({name}): scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
                Check($"unreadable locations ({name}): StubLock.T.Hit is {category}",
                    c.Records.Contains($"unresolved|StubLock.T.Hit|{category}"), Join(c.Records) + "\nSTDERR:\n" + r.Err);
            }
            finally
            {
                File.SetUnixFileMode(packages, dirMode);
                File.SetUnixFileMode(assets, fileMode);
            }
        }
    }

    // ------------------------------------------------------------------
    // ReferenceResolver.ResolveFrameworkReferences: targeting packs under a temp dotnet
    // root, net4x Reference Assemblies, and the carried-runtime fallback. Each case runs
    // under Isolated, whose iso-dotnet/iso-refasm dirs are pre-populated where needed.
    // ------------------------------------------------------------------

    private static string FwProject(string dir, string sdk, string tfm, string items = "") => WriteSource(dir, "App.csproj",
        $"<Project Sdk=\"{sdk}\">\n  <PropertyGroup><TargetFramework>{tfm}</TargetFramework></PropertyGroup>\n  <ItemGroup>{items}</ItemGroup>\n</Project>\n");

    /// ResolveFrameworkReferences(project, null, null): roots come from the overrides Isolated sets.
    private static List<MetadataReference> FrameworkRefs(string csproj)
        => (List<MetadataReference>)FrontendType("ReferenceResolver").GetMethod("ResolveFrameworkReferences")!
            .Invoke(null, new object?[] { ReadProject(csproj), null, null })!;

    private static IReadOnlyList<MetadataReference> CarriedRuntime()
        => (IReadOnlyList<MetadataReference>)FrontendType("CompilationLoader")
            .GetMethod("GetDefaultMetadataReferences", BindingFlags.NonPublic | BindingFlags.Static)!.Invoke(null, null)!;

    private static string RefPath(MetadataReference r) => ((PortableExecutableReference)r).FilePath!;

    /// <summary>
    /// With NETCore.App.Ref and AspNetCore.App.Ref packs at several versions: Microsoft.NET.Sdk
    /// resolves NETCore.App alone, Sdk.Web adds AspNetCore.App, Sdk.Razor resolves AspNetCore.App
    /// only with an explicit FrameworkReference; each pack is the highest version with the TFM's
    /// major.minor; the found set holds no carried-runtime reference and one System.Runtime.
    /// </summary>
    private static void TestFrameworkPackSelection(string baseDir)
    {
        var root = Path.GetFullPath(Path.Combine(baseDir, "fw-packs"));
        var runtime = StubAssembly("System.Runtime", "StubRuntime");
        var asp = StubAssembly("Microsoft.AspNetCore", "StubAsp");
        var carried = CarriedRuntime();
        var aspRef = "<FrameworkReference Include=\"Microsoft.AspNetCore.App\" />";
        var cases = new (string Name, string Sdk, string Tfm, string Items, string Core, string? Asp)[]
        {
            ("sdk", "Microsoft.NET.Sdk", "net8.0", "", "8.0.10", null),
            ("web", "Microsoft.NET.Sdk.Web", "net8.0", "", "8.0.10", "8.0.12"),
            ("web9", "Microsoft.NET.Sdk.Web", "net9.0", "", "9.0.0", "9.0.1"),
            ("razor", "Microsoft.NET.Sdk.Razor", "net8.0", "", "8.0.10", null),
            ("razor-explicit", "Microsoft.NET.Sdk.Razor", "net8.0", aspRef, "8.0.10", "8.0.12"),
        };
        foreach (var (name, sdk, tfm, items, core, aspVer) in cases)
        {
            var dir = Path.Combine(root, name);
            var packs = Path.Combine(dir, "iso-dotnet", "packs");
            string Pack(string fw, string ver, string t, string dll, byte[] image)
                => PutStub(Path.Combine(packs, fw + ".Ref", ver, "ref", t, dll), image);
            foreach (var v in new[] { "8.0.1", "8.0.9", "8.0.10", "9.0.0" })
            {
                Pack("Microsoft.NETCore.App", v, "net8.0", "System.Runtime.dll", runtime);
                if (v.StartsWith("9.", StringComparison.Ordinal)) Pack("Microsoft.NETCore.App", v, "net9.0", "System.Runtime.dll", runtime);
            }
            foreach (var v in new[] { "8.0.2", "8.0.12", "9.0.1" })
            {
                Pack("Microsoft.AspNetCore.App", v, "net8.0", "Microsoft.AspNetCore.dll", asp);
                if (v.StartsWith("9.", StringComparison.Ordinal)) Pack("Microsoft.AspNetCore.App", v, "net9.0", "Microsoft.AspNetCore.dll", asp);
            }
            var want = new List<string> { Path.GetFullPath(Path.Combine(packs, "Microsoft.NETCore.App.Ref", core, "ref", tfm, "System.Runtime.dll")) };
            if (aspVer != null) want.Add(Path.GetFullPath(Path.Combine(packs, "Microsoft.AspNetCore.App.Ref", aspVer, "ref", tfm, "Microsoft.AspNetCore.dll")));
            var csproj = FwProject(Path.Combine(dir, "App"), sdk, tfm, items);
            var refs = Isolated(dir, Path.Combine(dir, "nuget"), () => FrameworkRefs(csproj));
            var got = refs.Select(r => Path.GetFullPath(RefPath(r))).ToList();
            Check($"framework packs ({name}): {sdk} {tfm} resolves exactly NETCore.App {core}" + (aspVer == null ? " and no AspNetCore.App" : $" + AspNetCore.App {aspVer}"),
                got.SequenceEqual(want), "want:\n" + Join(want) + "\ngot:\n" + Join(got));
            Check($"framework packs ({name}): the found set replaces the carried runtime",
                !refs.Any(r => carried.Contains(r)), Join(got));
            Check($"framework packs ({name}): exactly one System.Runtime",
                refs.Count(r => Path.GetFileName(RefPath(r)) == "System.Runtime.dll") == 1, Join(got));
        }
    }

    /// <summary>
    /// net48: the Reference Assemblies root's .NETFramework/v4.8 dlls win over the
    /// microsoft.netframework.referenceassemblies package; with that root empty the package's
    /// build/.NETFramework/v4.8 in the global packages folder is used.
    /// </summary>
    private static void TestFrameworkNet4x(string baseDir)
    {
        var root = Path.GetFullPath(Path.Combine(baseDir, "fw-net4x"));
        var image = StubAssembly("mscorlib", "StubMscorlib");
        foreach (var name in new[] { "refasm", "package" })
        {
            var dir = Path.Combine(root, name);
            var nuget = Path.Combine(dir, "nuget");
            var pkg = PutStub(Path.Combine(nuget, "microsoft.netframework.referenceassemblies.net48", "1.0.3", "build", ".NETFramework", "v4.8", "mscorlib.dll"), image);
            var want = name == "refasm"
                ? new[] { "System.dll", "mscorlib.dll" }.Select(d => PutStub(Path.Combine(dir, "iso-refasm", ".NETFramework", "v4.8", d), image)).ToList()
                : new List<string> { pkg };
            var csproj = FwProject(Path.Combine(dir, "App"), "Microsoft.NET.Sdk", "net48");
            var got = Isolated(dir, nuget, () => FrameworkRefs(csproj)).Select(r => Path.GetFullPath(RefPath(r))).ToList();
            Check($"net48 framework references from the {name} location alone", got.SequenceEqual(want), "want:\n" + Join(want) + "\ngot:\n" + Join(got));
        }
    }

    /// <summary>
    /// A net7.0 project with no local reference assemblies (empty roots, empty NuGet folder) gets a
    /// new list of the carried runtime (non-empty, holding System.Runtime and System.Console), and a
    /// scan of its System.Console.WriteLine call emits that bound target categorized stdlib.
    /// </summary>
    private static void TestFrameworkCarriedFallback(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "fw-fallback"));
        var nuget = Path.Combine(dir, "nuget");
        var app = Path.Combine(dir, "App");
        var csproj = FwProject(app, "Microsoft.NET.Sdk", "net7.0");
        WriteSource(app, "Caller.cs", "namespace App;\n\npublic class Caller\n{\n    public void Go() { System.Console.WriteLine(\"x\"); }\n}\n");

        var carried = CarriedRuntime();
        var names = carried.Select(r => Path.GetFileName(RefPath(r))).ToList();
        Check("carried runtime is non-empty and holds System.Runtime and System.Console",
            carried.Count > 0 && names.Contains("System.Runtime.dll") && names.Contains("System.Console.dll"), Join(names));
        var refs = Isolated(dir, nuget, () => FrameworkRefs(csproj));
        Check("no local reference assemblies: a new list of exactly the carried runtime",
            !ReferenceEquals(refs, carried) && refs.SequenceEqual(carried), Join(refs.Select(RefPath)));

        var r = Isolated(dir, nuget, () => RunScanner(dir));
        var c = Canonicalize(ParseGraph(r.Out));
        var all = Join(c.Records) + "\nSTDERR:\n" + r.Err;
        Check("carried fallback scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        Check("carried fallback scan: System.Console.WriteLine is an unresolved_call categorized stdlib",
            c.Records.Contains("unresolved_call|App.Caller.Go|System.Console.WriteLine") && c.Records.Contains("unresolved|System.Console.WriteLine|stdlib"), all);
    }

    /// <summary>
    /// A Web net8.0 project whose dotnet root holds only Microsoft.AspNetCore.App.Ref 8.0 (no
    /// NETCore.App.Ref): a new list of the carried runtime (so its System.Runtime) followed by the
    /// found AspNetCore.App pack, never the AspNetCore pack alone.
    /// </summary>
    private static void TestFrameworkPartialPacks(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "fw-partial"));
        var asp = PutStub(Path.Combine(dir, "iso-dotnet", "packs", "Microsoft.AspNetCore.App.Ref", "8.0.12", "ref", "net8.0", "Microsoft.AspNetCore.dll"),
            StubAssembly("Microsoft.AspNetCore", "StubPartialAsp"));
        var csproj = FwProject(Path.Combine(dir, "App"), "Microsoft.NET.Sdk.Web", "net8.0");
        var carried = CarriedRuntime();
        var refs = Isolated(dir, Path.Combine(dir, "nuget"), () => FrameworkRefs(csproj));
        var got = refs.Select(r => Path.GetFullPath(RefPath(r))).ToList();
        Check("partial packs: missing NETCore.App keeps the carried runtime's System.Runtime",
            refs.Any(r => carried.Contains(r) && Path.GetFileName(RefPath(r)) == "System.Runtime.dll"), Join(got));
        Check("partial packs: a new list of the carried runtime followed by exactly the found AspNetCore.App pack",
            !ReferenceEquals(refs, carried) && refs.Take(carried.Count).SequenceEqual(carried) && got.Skip(carried.Count).SequenceEqual(new[] { asp }), Join(got));
    }

    /// Runs `body` with NUGET_PACKAGES = an empty dir, APG_CSHARP_DOTNET_ROOT / APG_CSHARP_REFASM_ROOT
    /// set to `dotnetEnv` / `refasmEnv` (never empty, so the default lookup never runs) and the static
    /// overrides set to the given values (null = consult the env); restores env and harness defaults after.
    private static T EnvRooted<T>(string nuget, string dotnetEnv, string refasmEnv, string? dotnetStatic, string? refasmStatic, Func<T> body)
    {
        var resolver = FrontendType("ReferenceResolver");
        var flags = BindingFlags.NonPublic | BindingFlags.Static;
        var prior = new[] { "APG_CSHARP_DOTNET_ROOT", "APG_CSHARP_REFASM_ROOT" }.ToDictionary(n => n, Environment.GetEnvironmentVariable);
        try
        {
            Environment.SetEnvironmentVariable("NUGET_PACKAGES", Directory.CreateDirectory(nuget).FullName);
            Environment.SetEnvironmentVariable("APG_CSHARP_DOTNET_ROOT", Directory.CreateDirectory(dotnetEnv).FullName);
            Environment.SetEnvironmentVariable("APG_CSHARP_REFASM_ROOT", Directory.CreateDirectory(refasmEnv).FullName);
            resolver.GetProperty("DotnetRootOverride", flags)!.SetValue(null, dotnetStatic);
            resolver.GetProperty("ReferenceAssembliesRootOverride", flags)!.SetValue(null, refasmStatic);
            return body();
        }
        finally
        {
            foreach (var (name, value) in prior) Environment.SetEnvironmentVariable(name, value);
            ApplyHarnessDefaults();
        }
    }

    /// <summary>
    /// With both static overrides null: APG_CSHARP_DOTNET_ROOT holding a NETCore.App.Ref pack resolves
    /// that pack from it alone, and at an empty dir yields the carried runtime; APG_CSHARP_REFASM_ROOT
    /// holding .NETFramework/v4.8 resolves net48 from it alone. A static override set beside a different
    /// env root wins: only the static root is read. Env vars and harness defaults are restored after.
    /// </summary>
    private static void TestEnvRootOverrides(string baseDir)
    {
        var priorDotnet = Environment.GetEnvironmentVariable("APG_CSHARP_DOTNET_ROOT");
        var priorRefasm = Environment.GetEnvironmentVariable("APG_CSHARP_REFASM_ROOT");
        var root = Path.GetFullPath(Path.Combine(baseDir, "fw-env-roots"));
        var nuget = Path.Combine(root, "nuget-empty");
        var runtime = StubAssembly("System.Runtime", "StubEnvRuntime");
        var mscorlib = StubAssembly("mscorlib", "StubEnvMscorlib");
        string Pack(string dotnet, string ver) => PutStub(Path.Combine(dotnet, "packs", "Microsoft.NETCore.App.Ref", ver, "ref", "net8.0", "System.Runtime.dll"), runtime);
        string RefAsm(string refasm, string dll) => PutStub(Path.Combine(refasm, ".NETFramework", "v4.8", dll), mscorlib);
        List<string> Paths(List<MetadataReference> refs) => refs.Select(r => Path.GetFullPath(RefPath(r))).ToList();
        var core = FwProject(Path.Combine(root, "core", "App"), "Microsoft.NET.Sdk", "net8.0");
        var net48 = FwProject(Path.Combine(root, "net48", "App"), "Microsoft.NET.Sdk", "net48");
        var emptyDotnet = Path.Combine(root, "env-dotnet-empty");
        var emptyRefasm = Path.Combine(root, "env-refasm-empty");

        var envDotnet = Path.Combine(root, "env-dotnet");
        var want = Pack(envDotnet, "8.0.5");
        var got = Paths(EnvRooted(nuget, envDotnet, emptyRefasm, null, null, () => FrameworkRefs(core)));
        Check("env roots: APG_CSHARP_DOTNET_ROOT's NETCore.App.Ref pack resolves from that root alone",
            got.SequenceEqual(new[] { want }), "want " + want + "\ngot:\n" + Join(got));

        var carried = CarriedRuntime();
        var refs = EnvRooted(nuget, emptyDotnet, emptyRefasm, null, null, () => FrameworkRefs(core));
        Check("env roots: APG_CSHARP_DOTNET_ROOT at an empty dir yields the carried runtime",
            refs.SequenceEqual(carried), Join(Paths(refs)));

        var envRefasm = Path.Combine(root, "env-refasm");
        var wantFw = new[] { "System.dll", "mscorlib.dll" }.Select(d => RefAsm(envRefasm, d)).ToList();
        got = Paths(EnvRooted(nuget, emptyDotnet, envRefasm, null, null, () => FrameworkRefs(net48)));
        Check("env roots: APG_CSHARP_REFASM_ROOT's .NETFramework/v4.8 resolves net48 from that root alone",
            got.SequenceEqual(wantFw), "want:\n" + Join(wantFw) + "\ngot:\n" + Join(got));

        var staticDotnet = Path.Combine(root, "static-dotnet");
        want = Pack(staticDotnet, "8.0.1");
        got = Paths(EnvRooted(nuget, envDotnet, emptyRefasm, Path.GetFullPath(staticDotnet), null, () => FrameworkRefs(core)));
        Check("env roots: a static dotnet-root override beats APG_CSHARP_DOTNET_ROOT",
            got.SequenceEqual(new[] { want }), "want " + want + "\ngot:\n" + Join(got));

        var staticRefasm = Path.Combine(root, "static-refasm");
        var wantStatic = RefAsm(staticRefasm, "mscorlib.dll");
        got = Paths(EnvRooted(nuget, emptyDotnet, envRefasm, null, Path.GetFullPath(staticRefasm), () => FrameworkRefs(net48)));
        Check("env roots: a static Reference Assemblies-root override beats APG_CSHARP_REFASM_ROOT",
            got.SequenceEqual(new[] { wantStatic }), "want " + wantStatic + "\ngot:\n" + Join(got));

        var resolver = FrontendType("ReferenceResolver");
        var flags = BindingFlags.NonPublic | BindingFlags.Static;
        Check("env roots: harness defaults and env vars restored afterwards",
            (string?)resolver.GetProperty("DotnetRootOverride", flags)!.GetValue(null) == HarnessDotnetRoot
            && (string?)resolver.GetProperty("ReferenceAssembliesRootOverride", flags)!.GetValue(null) == HarnessRefAsmRoot
            && Environment.GetEnvironmentVariable("NUGET_PACKAGES") == HarnessNugetPackages
            && Environment.GetEnvironmentVariable("APG_CSHARP_DOTNET_ROOT") == priorDotnet
            && Environment.GetEnvironmentVariable("APG_CSHARP_REFASM_ROOT") == priorRefasm, "");
    }

    /// <summary>
    /// A project referencing an unrestored package (no assets file, no packages/, empty global folder)
    /// scans to completion; every unbound reference is one as-written `unknown` target with no
    /// fragment/twin/nullable-suffixed target, and no process is spawned (sentinel shims on PATH and
    /// as the injected dotnet root's dotnet leave the marker dir empty).
    /// </summary>
    private static void TestUnrestoredPackageTargets(string baseDir)
    {
        var root = Path.GetFullPath(Path.Combine(baseDir, "unrestored-targets"));
        var dir = Path.Combine(root, "scan");
        var app = Path.Combine(dir, "App");
        var shims = Directory.CreateDirectory(Path.Combine(root, "shims")).FullName;
        var markers = Directory.CreateDirectory(Path.Combine(root, "markers")).FullName;
        // Isolated reuses <dir>/iso-dotnet as the dotnet-root override; seed it with the dotnet shim alone.
        var dotnetRoot = Directory.CreateDirectory(Path.Combine(dir, "iso-dotnet")).FullName;
        void Shim(string folder, string name)
        {
            // `: >` is a shell builtin: the shim must mark without any PATH lookup.
            var sh = WriteSource(folder, name, $"#!/bin/sh\n: > '{Path.Combine(markers, name)}'\n");
            if (OperatingSystem.IsWindows())
                WriteSource(folder, name + ".cmd", $"@type nul > \"{Path.Combine(markers, name)}\"\r\n");
            else
                File.SetUnixFileMode(sh, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
        }
        foreach (var name in new[] { "dotnet", "msbuild", "nuget" }) Shim(shims, name);
        Shim(dotnetRoot, "dotnet");

        RefProject(app, PkgRef("Unrestored.Pkg", "1.0.0"));
        WriteSource(app, "Caller.cs", """
            namespace App;

            public class Caller
            {
                public Unrestored.Pkg.Widget Field;
                public void Go() { Unrestored.Pkg.Widget.Make(); var w = new Unrestored.Pkg.Widget(); }
            }
            public class N { public Unrestored.Pkg.Widget? W; }
            public class G { public Foo<Bar> Gen; }
            public class L { public System.Collections.Generic.List<Bar> Lst; }
            public class X : Unrestored.Pkg.Base { }
            public class Y : Unrestored.Pkg.IThing { }
            """);

        var oldPath = Environment.GetEnvironmentVariable("PATH");
        (int Code, string Out, string Err) r;
        try
        {
            Environment.SetEnvironmentVariable("PATH", shims);
            r = Isolated(dir, Path.Combine(dir, "nuget-empty"), () => RunScanner(dir));
        }
        finally
        {
            Environment.SetEnvironmentVariable("PATH", oldPath);
        }

        var g = ParseGraph(r.Out);
        var c = Canonicalize(g);
        var all = Join(c.Records) + "\nSTDERR:\n" + r.Err;
        // Raw (non-deduplicated) unresolved edges, endpoints canonicalized: a twin edge must show up twice.
        var raw = g.Edges.Where(e => e.Type.StartsWith("unresolved_", StringComparison.Ordinal))
            .Select(e => $"{e.Type}|{(c.IdToFqn.TryGetValue(e.From, out var f) ? f : e.From)}|{e.To}").ToList();
        List<string> UsesFrom(string from) => raw.Where(s => s.StartsWith($"unresolved_use|{from}|", StringComparison.Ordinal))
            .Select(s => s.Split('|')[2]).OrderBy(s => s, StringComparer.Ordinal).ToList();

        Check("unrestored: scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        Check("unrestored: call, creation and field are unresolved edges to as-written unknown targets",
            c.Records.Contains("unresolved_call|App.Caller.Go|Unrestored.Pkg.Widget.Make")
            && c.Records.Contains("unresolved_call|App.Caller.Go|Unrestored.Pkg.Widget")
            && c.Records.Contains("unresolved_use|App.Caller|Unrestored.Pkg.Widget")
            && c.Records.Contains("unresolved|Unrestored.Pkg.Widget.Make|unknown")
            && c.Records.Contains("unresolved|Unrestored.Pkg.Widget|unknown"), all);
        Check("unrestored: one unresolved node per target name",
            g.Unres.Count == c.UnresFqns.Count, all);
        Check("unrestored: the member-access call is exactly one unresolved_call named as written",
            raw.Count(s => s == "unresolved_call|App.Caller.Go|Unrestored.Pkg.Widget.Make") == 1, Join(raw) + "\n" + all);
        var fragments = new[] { "Unrestored", "Pkg", "Widget", "Make", "Unrestored.Pkg", "Pkg.Widget", "Foo", "Unrestored.Pkg.Widget?" };
        Check("unrestored: no target for any qualified-name or member-access fragment, bare generic or nullable suffix",
            !fragments.Any(c.UnresFqns.Contains), all);
        Check("unrestored: the field's use is exactly the qualified name as written",
            UsesFrom("App.Caller").SequenceEqual(new[] { "Unrestored.Pkg.Widget" }), Join(raw));
        Check("unrestored: the base type is exactly one as-written use from X (no GetFullFqn twin)",
            UsesFrom("App.X").SequenceEqual(new[] { "Unrestored.Pkg.Base" }), Join(raw));
        Check("unrestored: the interface is exactly one as-written use from Y (no GetFullFqn twin)",
            UsesFrom("App.Y").SequenceEqual(new[] { "Unrestored.Pkg.IThing" }), Join(raw));
        Check("unrestored: Widget? lands on the one Unrestored.Pkg.Widget target",
            UsesFrom("App.N").SequenceEqual(new[] { "Unrestored.Pkg.Widget" }), Join(raw));
        Check("unrestored: Foo<Bar> yields exactly the targets Foo<Bar> and Bar",
            UsesFrom("App.G").SequenceEqual(new[] { "Bar", "Foo<Bar>" }), Join(raw));
        var listUses = UsesFrom("App.L").Distinct().ToList();
        Check("unrestored: List<Bar> yields Bar beside the bound List use",
            listUses.Count == 2 && listUses[0] == "Bar" && listUses[1].StartsWith("System.Collections.Generic.List", StringComparison.Ordinal)
            && c.Records.Contains($"unresolved|{listUses[1]}|stdlib"), Join(raw) + "\n" + all);
        Check("unrestored: the scan spawned no process (marker dir empty)",
            Directory.GetFileSystemEntries(markers).Length == 0, Join(Directory.GetFileSystemEntries(markers)));
        Check("unrestored: PATH restored after the scan", Environment.GetEnvironmentVariable("PATH") == oldPath, "");
    }

    /// <summary>
    /// Project and projectless sources naming Microsoft.CodeAnalysis types with no Roslyn reference stay
    /// unbound as-written `unknown` targets (the carried runtime holds none of the frontend's own Roslyn/Razor
    /// assemblies), while System.Console.WriteLine in the same sources binds as stdlib.
    /// </summary>
    private static void TestFrontendAssemblyIsolation(string baseDir)
    {
        var dir = Path.GetFullPath(Path.Combine(baseDir, "frontend-isolation"));
        const string body = "\n{\n    public Microsoft.CodeAnalysis.SyntaxTree Tree;\n"
            + "    public void Go() { Microsoft.CodeAnalysis.CSharp.CSharpSyntaxTree.ParseText(\"x\"); System.Console.WriteLine(\"x\"); }\n}\n";
        FwProject(Path.Combine(dir, "App"), "Microsoft.NET.Sdk", "net8.0");
        WriteSource(Path.Combine(dir, "App"), "Caller.cs", "namespace App;\n\npublic class Caller" + body);
        WriteSource(Path.Combine(dir, "Loose"), "Loose.cs", "namespace Loose;\n\npublic class L" + body);

        var leaked = CarriedRuntime().Select(r => Path.GetFileName(RefPath(r)))
            .Where(n => n.StartsWith("Microsoft.CodeAnalysis", StringComparison.Ordinal) || n.Contains("Razor", StringComparison.Ordinal)).ToList();
        Check("isolation: the carried runtime holds no Microsoft.CodeAnalysis or Razor assembly", leaked.Count == 0, Join(leaked));

        var r = Isolated(dir, Path.Combine(dir, "nuget-empty"), () => RunScanner(dir));
        var c = Canonicalize(ParseGraph(r.Out));
        var all = Join(c.Records) + "\nSTDERR:\n" + r.Err;
        const string tree = "Microsoft.CodeAnalysis.SyntaxTree", parse = "Microsoft.CodeAnalysis.CSharp.CSharpSyntaxTree.ParseText";
        Check("isolation: scan exits 0", r.Code == 0, "exit " + r.Code + "\n" + r.Err);
        foreach (var (owner, label) in new[] { ("App.Caller", "project"), ("Loose.L", "projectless") })
        {
            Check($"isolation ({label}): SyntaxTree field is an unresolved_use named as written",
                c.Records.Contains($"unresolved_use|{owner}|{tree}"), all);
            Check($"isolation ({label}): ParseText is an unresolved_call named as written",
                c.Records.Contains($"unresolved_call|{owner}.Go|{parse}"), all);
            Check($"isolation ({label}): System.Console.WriteLine is an unresolved_call",
                c.Records.Contains($"unresolved_call|{owner}.Go|System.Console.WriteLine"), all);
        }
        Check("isolation: Roslyn targets are category unknown (never stdlib/external)",
            c.Records.Contains($"unresolved|{tree}|unknown") && c.Records.Contains($"unresolved|{parse}|unknown")
            && !c.Records.Any(s => s.StartsWith("unresolved|Microsoft.CodeAnalysis", StringComparison.Ordinal) && !s.EndsWith("|unknown", StringComparison.Ordinal)), all);
        Check("isolation: no resolved edge or node for any Microsoft.CodeAnalysis symbol",
            !c.Records.Any(s => (s.StartsWith("calls|", StringComparison.Ordinal) || s.StartsWith("uses|", StringComparison.Ordinal)) && s.Contains("Microsoft.CodeAnalysis", StringComparison.Ordinal))
            && !c.NodeFqns.Any(f => f.StartsWith("Microsoft.CodeAnalysis", StringComparison.Ordinal)), all);
        Check("isolation: System.Console.WriteLine is categorized stdlib (bound against the carried runtime)",
            c.Records.Contains("unresolved|System.Console.WriteLine|stdlib"), all);
        Check("isolation: NUGET_PACKAGES restored to the harness default",
            Environment.GetEnvironmentVariable("NUGET_PACKAGES") == HarnessNugetPackages, "");
    }

    /// <summary>
    /// Two projects A and B compiled back to back through CreateProjectCompilation, A with a
    /// referenced compilation C: B holds no CompilationReference to C or A, A holds C's exactly
    /// once, and the carried set is the same instances at the same count before and after.
    /// Runs at the harness defaults (no Isolated swap).
    /// </summary>
    private static void TestCachedReferenceIsolation(string baseDir)
    {
        var root = Path.GetFullPath(Path.Combine(baseDir, "cached-refs"));
        var create = FrontendType("CompilationLoader").GetMethod("CreateProjectCompilation")!;
        CSharpCompilation Compile(string name, IEnumerable<CSharpCompilation> referenced)
        {
            var dir = Path.Combine(root, name);
            var csproj = FwProject(dir, "Microsoft.NET.Sdk", "net8.0");
            var src = WriteSource(dir, name + ".cs", $"namespace {name};\n\npublic class {name}T {{ }}\n");
            var trees = new List<SyntaxTree> { CSharpSyntaxTree.ParseText(File.ReadAllText(src), path: src) };
            return (CSharpCompilation)create.Invoke(null, new object[] { ReadProject(csproj), root, trees, referenced })!;
        }

        var before = CarriedRuntime().ToList();
        var c = CSharpCompilation.Create("C", new[] { CSharpSyntaxTree.ParseText("namespace C; public class CT { }") });
        var a = Compile("A", new[] { c });
        var b = Compile("B", Array.Empty<CSharpCompilation>());
        var after = CarriedRuntime().ToList();

        static string Names(CSharpCompilation x) => Join(x.References.Select(r => r.Display ?? r.GetType().Name));
        Check("cached refs: B holds no CompilationReference to C or A",
            !b.References.OfType<CompilationReference>().Any(r => r.Compilation == c || r.Compilation == a), Names(b));
        Check("cached refs: A holds C's CompilationReference exactly once",
            a.References.OfType<CompilationReference>().Count(r => r.Compilation == c) == 1, Names(a));
        Check("cached refs: carried set count unchanged", before.Count == after.Count, $"{before.Count} -> {after.Count}");
        Check("cached refs: carried set is the same instances",
            before.Count == after.Count && before.Zip(after).All(p => ReferenceEquals(p.First, p.Second)), "");
    }

    /// <summary>
    /// Shared resolver-reference cache: A and B naming one HintPath dll (B through a `..` segment)
    /// and one temp dotnet root's net8.0 pack get the same MetadataReference instances in distinct
    /// lists, MetadataReferenceCache.Get is idempotent per path, and CreateProjectCompilation for A
    /// and B shares those instances. Runs under Isolated (empty NUGET_PACKAGES, temp roots).
    /// </summary>
    private static void TestSharedReferenceCache(string baseDir)
    {
        var root = Path.GetFullPath(Path.Combine(baseDir, "shared-refs"));
        var hint = PutStub(Path.Combine(root, "lib", "Stub.Shared.dll"), StubAssembly("Stub.Shared", "StubShared"));
        var packDir = Path.Combine(root, "iso-dotnet", "packs", "Microsoft.NETCore.App.Ref", "8.0.0", "ref", "net8.0");
        PutStub(Path.Combine(packDir, "System.Runtime.dll"), StubAssembly("System.Runtime", "StubRuntime"));
        PutStub(Path.Combine(packDir, "System.Extra.dll"), StubAssembly("System.Extra", "StubExtra"));
        string Proj(string name, string hintPath) => FwProject(Path.Combine(root, name), "Microsoft.NET.Sdk", "net8.0",
            $"<Reference Include=\"Stub.Shared\"><HintPath>{hintPath}</HintPath></Reference>");
        var a = Proj("A", "..\\lib\\Stub.Shared.dll");
        var b = Proj("B", "..\\B\\..\\lib\\Stub.Shared.dll");
        var resolver = FrontendType("ReferenceResolver");
        var get = FrontendType("MetadataReferenceCache").GetMethod("Get")!;
        var create = FrontendType("CompilationLoader").GetMethod("CreateProjectCompilation")!;
        List<MetadataReference> Resolve(string csproj) => (List<MetadataReference>)resolver.GetMethod("Resolve")!.Invoke(null, new object[] { ReadProject(csproj), root })!;
        CSharpCompilation Compile(string csproj) => (CSharpCompilation)create.Invoke(null, new object[]
            { ReadProject(csproj), root, new List<SyntaxTree> { CSharpSyntaxTree.ParseText("namespace X; public class T { }") }, Array.Empty<CSharpCompilation>() })!;

        Isolated(root, Path.Combine(root, "nuget"), () =>
        {
            var ra = Resolve(a);
            var rb = Resolve(b);
            MetadataReference? Hint(List<MetadataReference> refs) => refs.FirstOrDefault(r => Path.GetFullPath(RefPath(r)) == hint);
            var ha = Hint(ra);
            Check("shared refs: A and B resolve the HintPath dll to the same instance",
                ha != null && ReferenceEquals(ha, Hint(rb)), Join(ra.Concat(rb).Select(RefPath)));
            Check("shared refs: A's and B's Resolve lists are distinct objects", !ReferenceEquals(ra, rb), "");
            int bCount = rb.Count;
            ra.Add(MetadataReference.CreateFromImage(StubAssembly("Stub.Extra", "StubExtraAdded")));
            var rc = Resolve(a);
            Check("shared refs: adding to A's list leaves B's list and a third Resolve unchanged",
                rb.Count == bCount && rc.Count == ra.Count - 1 && ReferenceEquals(Hint(rc), ha), $"B {bCount}->{rb.Count}, third {rc.Count}, A {ra.Count}");

            var fa = FrameworkRefs(a);
            var fb = FrameworkRefs(b);
            Check("shared refs: the net8.0 pack resolves both dlls", fa.Count == 2 && fa.All(r => RefPath(r).StartsWith(packDir, StringComparison.Ordinal)), Join(fa.Select(RefPath)));
            Check("shared refs: A's and B's pack references are the same instances element for element",
                fa.Count == fb.Count && fa.Zip(fb).All(p => ReferenceEquals(p.First, p.Second)), Join(fa.Concat(fb).Select(RefPath)));
            Check("shared refs: A's and B's framework lists are distinct objects", !ReferenceEquals(fa, fb), "");

            Check("shared refs: MetadataReferenceCache.Get twice returns the same instance",
                ReferenceEquals(get.Invoke(null, new object[] { hint }), get.Invoke(null, new object[] { hint })), "");

            var ca = Compile(a);
            var cb = Compile(b);
            foreach (var shared in fa.Append(ha!))
                Check($"shared refs: both compilations hold {Path.GetFileName(RefPath(shared))} as one instance",
                    ca.References.Count(r => ReferenceEquals(r, shared)) == 1 && cb.References.Count(r => ReferenceEquals(r, shared)) == 1,
                    Join(ca.References.Concat(cb.References).Select(r => r.Display ?? "")));
            return 0;
        });
        Check("shared refs: NUGET_PACKAGES restored to the harness default",
            Environment.GetEnvironmentVariable("NUGET_PACKAGES") == HarnessNugetPackages, "");
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
