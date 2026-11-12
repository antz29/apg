using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace Apg.CsharpFrontend;

public static class Program
{
    public static int Main(string[] args)
    {
        if (args.Length == 0 || args[0] == "-h" || args[0] == "--help")
        {
            Console.Error.WriteLine("Usage: csharpfrontend <project_dir> [--module <dir>]... [--id-prefix <p>] [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [--razor <abs-path>]... [exclude_globs...]");
            return 1;
        }

        string rootDir = Path.GetFullPath(args[0]);
        var moduleDirs = new List<string>();
        var excludePatterns = new List<string>();
        // `--id-prefix <p>` (default "n") keeps opaque ids unique across
        // frontends when a scan merges multiple languages (each frontend
        // starts its counter at `n1`).
        string idPrefix = "n";
        // The phase-02 target-set hand-off (the pinned interface): `--targets`
        // is an emission filter, `--cache-dir`/`--cache-key` locate the shared
        // per-language artifact directory.
        string? targetsPath = null;
        string? cacheDir = null;
        string? cacheKey = null;
        // Spike-only explicit Razor inputs (`--razor <abs-path>`, repeatable);
        // transitional until Razor runs through Scanner.Run.
        var razorPaths = new List<string>();

        int i = 1;
        while (i < args.Length)
        {
            if (args[i] == "--module")
            {
                i++;
                if (i < args.Length)
                {
                    moduleDirs.Add(Path.GetFullPath(Path.Combine(rootDir, args[i])));
                }
            }
            else if (args[i] == "--id-prefix")
            {
                i++;
                if (i < args.Length)
                {
                    idPrefix = args[i];
                }
            }
            else if (args[i] == "--targets")
            {
                i++;
                if (i < args.Length)
                {
                    targetsPath = args[i];
                }
            }
            else if (args[i] == "--cache-dir")
            {
                i++;
                if (i < args.Length)
                {
                    cacheDir = args[i];
                }
            }
            else if (args[i] == "--cache-key")
            {
                i++;
                if (i < args.Length)
                {
                    cacheKey = args[i];
                }
            }
            else if (args[i] == "--razor")
            {
                i++;
                // A dangling `--razor` still selects Razor mode and fails as a missing path.
                razorPaths.Add(i < args.Length ? args[i] : "");
            }
            else
            {
                excludePatterns.Add(args[i]);
            }
            i++;
        }

        if (razorPaths.Count > 0)
        {
            try
            {
                var engine = RazorCompiler.CreateProjectEngine(rootDir);
                var compilation = CompilationLoader.CreateCompilation(RazorCompiler.CompileDeclarations(engine, null, rootDir, razorPaths));
                var modules = new HashSet<string>(StringComparer.Ordinal);
                int nextId = 0;
                foreach (var razorPath in razorPaths)
                {
                    if (!File.Exists(razorPath)) throw new FileNotFoundException($"Razor input not found: {razorPath}");
                    var razorText = File.ReadAllText(razorPath);
                    var razorLines = Microsoft.CodeAnalysis.Text.SourceText.From(razorText).Lines;
                    var (generated, errors) = RazorCompiler.Compile(engine, null, rootDir, [razorPath], compilation).Single();
                    if (errors.Count > 0)
                        throw new InvalidOperationException($"Razor compile failed for {razorPath}: {string.Join("; ", errors)}");
                    var typeDecl = generated.GetRoot().DescendantNodes().OfType<ClassDeclarationSyntax>().First();
                    string ns = typeDecl.Ancestors().OfType<BaseNamespaceDeclarationSyntax>().FirstOrDefault()?.Name.ToString() ?? "";

                    string prevModule = "";
                    foreach (var part in ns.Split('.', StringSplitOptions.RemoveEmptyEntries))
                    {
                        string module = prevModule.Length == 0 ? part : $"{prevModule}.{part}";
                        if (modules.Add(module))
                        {
                            JsonlWriter.Write(new ModuleMsg { Fqn = module });
                            if (prevModule.Length > 0) JsonlWriter.Write(new EdgeMsg { Type = "contains", From = prevModule, To = module });
                        }
                        prevModule = module;
                    }

                    JsonlWriter.Write(new FileMsg { Path = razorPath, Parent = ns, StartLine = 1, EndLine = Math.Max(1, razorLines.Count) });

                    // The generated type stands for the whole Razor file.
                    string typeId = $"{idPrefix}{++nextId}";
                    string typeName = typeDecl.Identifier.Text;
                    JsonlWriter.Write(new StructMsg
                    {
                        Id = typeId, Parent = ns, Name = typeName, Path = razorPath,
                        Start = 0, End = razorText.Length, StartLine = 1, EndLine = Math.Max(1, razorLines.Count)
                    });

                    foreach (var method in typeDecl.Members.OfType<MethodDeclarationSyntax>())
                    {
                        // Only @code/@functions methods #line-map into the Razor file; generated plumbing does not.
                        var head = RazorSourceMap.MapSpan(generated, method.Identifier.Span);
                        if (head is not { } h || h.Path != razorPath) continue;
                        var whole = RazorSourceMap.MapSpan(generated, method.Span);
                        int endLine = whole is { } w && w.Path == razorPath && w.EndLine >= h.StartLine ? w.EndLine : h.StartLine;
                        string funcId = $"{idPrefix}{++nextId}";
                        JsonlWriter.Write(new FuncMsg
                        {
                            Id = funcId, Parent = string.IsNullOrEmpty(ns) ? typeName : $"{ns}.{typeName}",
                            Name = method.Identifier.Text,
                            Params = method.ParameterList.Parameters.Select(p => p.Type?.ToString() ?? "").ToList(),
                            File = razorPath, Path = razorPath,
                            Start = razorLines[h.StartLine - 1].Start, End = razorLines[endLine - 1].End,
                            StartLine = h.StartLine, EndLine = endLine
                        });
                        JsonlWriter.Write(new EdgeMsg { Type = "contains", From = typeId, To = funcId });
                    }
                }
                return 0;
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"csharpfrontend error: {ex.Message}");
                return 1;
            }
        }

        // The pinned per-language artifact location is
        // `<cache-dir>/csharp/<cache-key>/` — the directory the shared
        // content-addressed fact store keys its C# units under. Create it up
        // front so the location exists and is shared across scans/worktrees.
        EnsureArtifactDir(cacheDir, cacheKey);

        // The target set is an EMISSION filter only. An absent flag, an empty
        // file, or a file that cannot be read yields "no filter" (the
        // byte-identical full-scan path) — never "emit nothing".
        var targetFiles = ReadTargetSet(targetsPath);

        try
        {
            var scanner = new Scanner(rootDir, moduleDirs, excludePatterns, idPrefix, targetFiles);
            scanner.Run();
            return 0;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"csharpfrontend error: {ex}");
            return 1;
        }
    }

    /// Creates the pinned per-language artifact directory
    /// (`<cache-dir>/csharp/<cache-key>/`) when the hand-off supplies a store
    /// root. Roslyn resolves against a single in-process compilation, so the
    /// directory carries no compiler artifact of its own; the shared fact store
    /// keys the per-file units there.
    private static void EnsureArtifactDir(string? cacheDir, string? cacheKey)
    {
        if (string.IsNullOrEmpty(cacheDir)) return;
        try
        {
            var dir = Path.Combine(cacheDir, "csharp");
            if (!string.IsNullOrEmpty(cacheKey)) dir = Path.Combine(dir, cacheKey);
            Directory.CreateDirectory(dir);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Warning: could not create C# artifact dir under {cacheDir}: {ex.Message}");
        }
    }

    /// Reads the pinned `--targets` list: a UTF-8, newline-delimited file of
    /// absolute source-file paths, one per line, no header; blank (and
    /// whitespace-only) lines are ignored. A missing/unreadable file warns and
    /// yields the empty set, which the caller treats as "no filter".
    private static HashSet<string> ReadTargetSet(string? path)
    {
        var set = new HashSet<string>(StringComparer.Ordinal);
        if (string.IsNullOrEmpty(path)) return set;
        try
        {
            foreach (var raw in File.ReadLines(path, Encoding.UTF8))
            {
                var line = raw.Trim();
                if (line.Length == 0) continue;
                set.Add(Path.GetFullPath(line));
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Warning: could not read targets {path}: {ex.Message}");
        }
        return set;
    }
}

/// <summary>
/// Scan orchestration and shared emission state. The pass emitters
/// (<see cref="DeclarationEmitter"/> / <see cref="EdgeEmitter"/>) take this
/// Scanner as a collaborator and use the internal accessors below for id
/// allocation, the symbol/decl maps, module registration, and unresolved
/// marking. The state stays on Scanner so the test harness's reflection over
/// <c>_projectSymbols</c> / <c>CanonicalFqn</c> is preserved.
/// </summary>
public class Scanner
{
    private readonly string _rootDir;
    private readonly List<string> _moduleDirs;
    private readonly List<string> _excludePatterns;
    private readonly string _idPrefix;

    // The target-set emission filter (the pinned phase-02 hand-off). Non-empty
    // means only the target files' per-file facts are emitted; the full
    // compilation is still built so every reference resolves exactly.
    private readonly HashSet<string> _targetFiles;
    private readonly bool _filtered;

    // Filtered-scan support, populated in a registration prepass over the whole
    // project (all files, target or not): every declared project symbol (so a
    // cross-file edge endpoint declared in a non-emitted file is recognised as
    // project code, not a foreign reference) and the `(parent,name)` groups the
    // ingestor renders with an overload suffix.
    private readonly HashSet<ISymbol> _projectSymbols = new(SymbolEqualityComparer.Default);
    private readonly Dictionary<string, int> _methodGroups = new();

    private int _nodeCounter = 0;
    private readonly object _lock = new();

    private readonly Dictionary<ISymbol, string> _symbolToId = new(SymbolEqualityComparer.Default);
    private readonly Dictionary<string, string> _declFqnToId = new();
    private readonly HashSet<string> _emittedModules = new();
    private readonly HashSet<string> _emittedUnresolved = new();

    public Scanner(string rootDir, List<string> moduleDirs, List<string> excludePatterns, string idPrefix,
                   HashSet<string>? targetFiles = null)
    {
        _rootDir = rootDir;
        _moduleDirs = moduleDirs;
        _excludePatterns = excludePatterns;
        _idPrefix = idPrefix;
        _targetFiles = targetFiles ?? new HashSet<string>(StringComparer.Ordinal);
        _filtered = _targetFiles.Count > 0;
    }

    internal string NextId()
    {
        lock (_lock)
        {
            _nodeCounter++;
            return $"{_idPrefix}{_nodeCounter}";
        }
    }

    public void Run()
    {
        var discovery = new SourceDiscovery(_moduleDirs, _excludePatterns);
        var files = discovery.DiscoverFiles(_rootDir).ToList();
        if (files.Count == 0)
        {
            Console.Error.WriteLine("No .cs, .csx, .cshtml or .razor files found to scan.");
            CollisionQualifier.Compute([]); // every scan replaces an earlier in-process scan's map
            return;
        }

        Console.Error.WriteLine($"Scanning {files.Count} C# source files...");

        // Project discovery: a dedicated .sln/.csproj walk under the root with
        // the same skip/scope predicate as source discovery; projects are
        // de-duplicated by full path (walk- and .sln-reached alike). An
        // unreadable .sln/.csproj warns and is skipped, never failing the scan.
        var projectPaths = new SortedSet<string>(StringComparer.Ordinal);
        var solutions = new List<string>();
        var dirs = new Stack<string>();
        dirs.Push(_rootDir);
        while (dirs.Count > 0)
        {
            var dir = dirs.Pop();
            try
            {
                foreach (var f in Directory.GetFiles(dir))
                {
                    if (discovery.IsExcluded(f, false)) continue;
                    if (f.EndsWith(".csproj", StringComparison.OrdinalIgnoreCase)) projectPaths.Add(Path.GetFullPath(f));
                    else if (f.EndsWith(".sln", StringComparison.OrdinalIgnoreCase)) solutions.Add(f);
                }
                foreach (var d in Directory.GetDirectories(dir))
                {
                    if (!discovery.IsExcluded(d, true)) dirs.Push(d);
                }
            }
            catch
            {
                // Unreadable directory: skipped, as in source discovery.
            }
        }
        // A .sln-listed project gets the walk's scope: under the root, with no
        // skipped directory between the root and it.
        var scanRoot = Path.TrimEndingDirectorySeparator(Path.GetFullPath(_rootDir));
        bool InWalkScope(string p)
        {
            var full = Path.GetFullPath(p);
            if (!full.StartsWith(scanRoot + Path.DirectorySeparatorChar, StringComparison.Ordinal) || discovery.IsExcluded(full, false)) return false;
            for (var d = Path.GetDirectoryName(full)!; d.Length > scanRoot.Length; d = Path.GetDirectoryName(d)!)
            {
                if (discovery.IsExcluded(d, true)) return false;
            }
            return true;
        }
        foreach (var sln in solutions)
        {
            try
            {
                foreach (var p in SolutionReader.Read(sln))
                {
                    if (InWalkScope(p)) projectPaths.Add(p);
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Warning: could not read solution {sln}: {ex.Message}");
            }
        }
        var projects = new Dictionary<string, CsharpProject>(StringComparer.Ordinal);
        foreach (var path in projectPaths)
        {
            try
            {
                projects[path] = ProjectFileReader.Read(path);
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Warning: could not read project {path}: {ex.Message}");
            }
        }
        // The one owning project per project source file; projectless .cs and .csx have no entry.
        var owners = SourceOwnership.AssignOwners(projects.Values);

        // Loaded projects a project references (as written, relative to its
        // directory); a missing or unloaded reference is dropped.
        IEnumerable<CsharpProject> References(CsharpProject p) => p.ProjectReferences
            .Select(r => Path.GetFullPath(Path.Combine(Path.GetDirectoryName(p.Path)!, r.Replace('\\', Path.DirectorySeparatorChar))))
            .Distinct()
            .Where(projects.ContainsKey)
            .Select(r => projects[r]);

        // Project-reference (topological) order; a reference cycle is cut at
        // its back edge, so that reference is simply not a compilation reference.
        var order = new List<CsharpProject>();
        var visited = new HashSet<string>(StringComparer.Ordinal);
        void Visit(CsharpProject p)
        {
            if (!visited.Add(p.Path)) return;
            foreach (var r in References(p)) Visit(r);
            order.Add(p);
        }
        foreach (var p in projects.Values.OrderBy(p => p.Path, StringComparer.Ordinal)) Visit(p);

        // Each project compilation uses its resolved framework, package and
        // assembly references, with the carried runtime as fallback; the
        // projectless compilation and each per-script .csx compilation keep
        // the carried-runtime references. One compilation per
        // project over its discovered files (SourcePaths, plus the Razor files
        // under its directory so its compilation holds them back), one
        // projectless compilation over every other .cs/.cshtml/.razor file,
        // and one per .csx script over exactly that script's tree, so a script
        // binds only its own declarations (scripts sharing one compilation
        // would merge into one Script class). Each compilation covers its FULL
        // set: the target set filters emission, not resolution. A file
        // compiled by several projects emits from its owner's compilation
        // only; held-back Razor is the union, emitted once.
        static bool IsRazor(string f) =>
            f.EndsWith(".cshtml", StringComparison.OrdinalIgnoreCase) || f.EndsWith(".razor", StringComparison.OrdinalIgnoreCase);
        var nonScripts = files.Where(f => !f.EndsWith(".csx", StringComparison.OrdinalIgnoreCase)).ToList();
        var owned = new HashSet<string>(StringComparer.Ordinal);
        var heldBack = new HashSet<string>(StringComparer.Ordinal);
        var byPath = new Dictionary<string, (SyntaxTree Tree, SemanticModel Model, CompilationUnitSyntax Root)>(StringComparer.Ordinal);
        var compiled = new Dictionary<string, CSharpCompilation>(StringComparer.Ordinal);
        foreach (var p in order)
        {
            var sources = new HashSet<string>(p.SourcePaths, StringComparer.Ordinal);
            var projectDir = Path.GetDirectoryName(p.Path)! + Path.DirectorySeparatorChar;
            var mine = nonScripts.Where(f => IsRazor(f) ? f.StartsWith(projectDir, StringComparison.Ordinal) : sources.Contains(f)).ToList();
            owned.UnionWith(mine);
            var version = p.LanguageVersion is { } lv && LanguageVersionFacts.TryParse(lv, out var parsed) ? parsed : LanguageVersion.Latest;
            var options = CSharpParseOptions.Default.WithLanguageVersion(version).WithPreprocessorSymbols(p.PreprocessorSymbols);
            var trees = CompilationLoader.ParseTrees(mine, out var held, options);
            heldBack.UnionWith(held);
            var compilation = CompilationLoader.CreateProjectCompilation(p, scanRoot, trees,
                References(p).Where(r => compiled.ContainsKey(r.Path)).Select(r => compiled[r.Path]).ToList());
            compiled[p.Path] = compilation;
            foreach (var u in CompilationLoader.BuildModels(compilation, trees))
            {
                if (owners.TryGetValue(u.Tree.FilePath, out var o) && o == p) byPath[u.Tree.FilePath] = u;
            }
        }
        var loose = nonScripts.Where(f => !owned.Contains(f)).ToList();
        var looseTrees = CompilationLoader.ParseTrees(loose, out var looseHeld);
        heldBack.UnionWith(looseHeld);
        foreach (var u in CompilationLoader.BuildModels(CompilationLoader.CreateCompilation(looseTrees), looseTrees)) byPath.TryAdd(u.Tree.FilePath, u);
        var scripts = files.Where(f => f.EndsWith(".csx", StringComparison.OrdinalIgnoreCase)).ToList();
        foreach (var t in CompilationLoader.ParseTrees(scripts, out _))
        {
            var one = new List<SyntaxTree> { t };
            foreach (var u in CompilationLoader.BuildModels(CompilationLoader.CreateCompilation(one), one)) byPath.TryAdd(u.Tree.FilePath, u);
        }

        // An all-targets request (every discovered file selected) is the full
        // path, so it stays byte-identical to a plain full scan.
        bool filtered = _filtered && !files.All(f => _targetFiles.Contains(f));

        // One semantic model per file, from its emitting compilation, joined
        // back in discovery order.
        var unitModels = files.Where(byPath.ContainsKey).Select(f => byPath[f]).ToList();

        // Collision qualification over every emitted unit (target or not), before
        // any naming, so a filtered scan names exactly as the full scan does.
        CollisionQualifier.Compute(unitModels.Select(u => (u.Model, u.Root, owners.GetValueOrDefault(u.Tree.FilePath))));

        if (filtered)
        {
            // Register the whole project's declaration universe and overload
            // groups before emitting, so a target file's edge to a symbol
            // declared in a non-emitted file lands on that symbol's canonical
            // FQN (the cached unit supplies the node).
            Console.Error.WriteLine($"Target set in force: emitting facts for {files.Count(f => IsTargetFile(f))} of {files.Count} files.");
            RegisterUniverse(unitModels, owners, compiled);
        }

        // Pass 1: Collect declarations & emit nodes. Module scaffolding is
        // global (it is not part of a per-file fact unit), so a non-target file
        // still contributes its module hierarchy; only its declarations and
        // edges are withheld.
        var declarations = new DeclarationEmitter(this);
        declarations.EmitProjectModules(solutions, projects.Values);
        foreach (var (tree, model, root) in unitModels)
        {
            declarations.EmitFileAndDeclarations(tree, model, root, emit: !filtered || IsTargetFile(tree.FilePath),
                owner: owners.GetValueOrDefault(tree.FilePath));
        }

        // Held-back .cshtml/.razor files stay graphed as file-only nodes (no
        // namespace, so the root parent a namespace-less .cs file gets).
        foreach (var path in files.Where(heldBack.Contains))
        {
            if (filtered && !IsTargetFile(path)) continue;
            var text = Microsoft.CodeAnalysis.Text.SourceText.From(File.ReadAllText(path, Encoding.UTF8));
            JsonlWriter.Write(new FileMsg { Path = path, Parent = "", StartLine = 1, EndLine = Math.Max(1, text.Lines.Count) });
        }

        // Pass 2: Semantic walk for edges (calls & uses) — target files only
        // when a filter is in force.
        var edges = new EdgeEmitter(this);
        foreach (var (tree, model, root) in unitModels)
        {
            if (!filtered || IsTargetFile(tree.FilePath))
            {
                edges.EmitEdges(tree, model, root);
            }
        }
    }

    /// True when `path` is in the target set (always true with no filter).
    private bool IsTargetFile(string path) => !_filtered || _targetFiles.Contains(path);

    /// Filtered-scan registration prepass over every compilation: records
    /// each declared project symbol and counts each method-like declaration's
    /// `(parent,name)` group so the canonical FQN carries the ingestor's
    /// overload suffix. `units` holds one entry per emitted file, bound to the
    /// one compilation that file is emitted from, and only each unit's own
    /// syntax root is walked — so a file compiled by several projects counts
    /// its methods once and the group sizes match the emitted records.
    /// With the SourceOwnership.AssignOwners result (`owners`) and the project
    /// compilations by project path (`compiled`), a project file registers
    /// only from its owner's compilation; a file with no owner (projectless
    /// .cs, or a .csx script) registers from its one compilation.
    private void RegisterUniverse(List<(SyntaxTree Tree, SemanticModel Model, CompilationUnitSyntax Root)> units,
                                  Dictionary<string, CsharpProject>? owners = null,
                                  Dictionary<string, CSharpCompilation>? compiled = null)
    {
        foreach (var (tree, model, root) in units)
        {
            if (owners != null && compiled != null && owners.TryGetValue(tree.FilePath, out var owner)
                && (!compiled.TryGetValue(owner.Path, out var ownerCompilation) || model.Compilation != ownerCompilation)) continue;
            foreach (var member in root.DescendantNodes())
            {
                if (member is BaseTypeDeclarationSyntax typeDecl)
                {
                    var sym = model.GetDeclaredSymbol(typeDecl);
                    if (sym != null) _projectSymbols.Add(sym);
                }
                else if (member is MethodDeclarationSyntax methodDecl)
                {
                    var sym = model.GetDeclaredSymbol(methodDecl);
                    if (sym != null)
                    {
                        _projectSymbols.Add(sym);
                        CountMethodGroup(sym);
                    }
                }
                else if (member is ConstructorDeclarationSyntax ctorDecl)
                {
                    var sym = model.GetDeclaredSymbol(ctorDecl);
                    if (sym != null)
                    {
                        _projectSymbols.Add(sym);
                        CountMethodGroup(sym);
                    }
                }
                else if (member is PropertyDeclarationSyntax propDecl)
                {
                    var propSym = model.GetDeclaredSymbol(propDecl);
                    if (propSym == null) continue;
                    if (propSym.GetMethod != null)
                    {
                        _projectSymbols.Add(propSym.GetMethod);
                        CountMethodGroup(propSym.GetMethod);
                    }
                    if (propSym.SetMethod != null)
                    {
                        _projectSymbols.Add(propSym.SetMethod);
                        CountMethodGroup(propSym.SetMethod);
                    }
                }
            }
        }
    }

    private void CountMethodGroup(IMethodSymbol method)
    {
        string key = SymbolNaming.MethodGroupKey(method);
        _methodGroups[key] = _methodGroups.TryGetValue(key, out var count) ? count + 1 : 1;
    }

    /// The canonical FQN the ingestor renders for a symbol declared in this
    /// project, computed from the same `parent`/`name`/`params` the frontend
    /// emits (SPEC §4 overload rendering).
    private string CanonicalFqn(ISymbol symbol)
    {
        if (symbol is IMethodSymbol method)
        {
            string parent = SymbolNaming.GetParentScopeFqn(method);
            string name = SymbolNaming.EmittedMethodName(method);
            string fqn = $"{parent}.{name}";
            if (_methodGroups.TryGetValue(SymbolNaming.MethodGroupKey(method), out var group) && group > 1)
            {
                var ps = method.Parameters.Select(SymbolNaming.GetParameterTypeString);
                fqn += $"({string.Join(",", ps)})";
            }
            return fqn;
        }
        // A type renders `parent.name`, where `parent` is exactly what the
        // emitted struct record carries (the containing type's FQN, the
        // collision-qualified project scope, or the namespace); an empty
        // (global-namespace) parent renders the bare name, never `.name`.
        string typeParent = SymbolNaming.GetParentScopeFqn(symbol);
        string typeName = SymbolNaming.GetTypeNameWithArity((INamedTypeSymbol)symbol);
        return typeParent.Length == 0 ? typeName : $"{typeParent}.{typeName}";
    }

    /// Resolves an edge endpoint: the opaque id when the target symbol is part
    /// of the emitted (target) set, else the canonical FQN when it is project
    /// code declared in a non-emitted file (the cached unit supplies its node).
    /// A shared file's declaration seen from any compiling project maps by its
    /// declaration site to the one emitted/registered symbol — the owning
    /// project's — so every project's references land on one identity.
    /// Returns false for a foreign symbol (external/stdlib), which becomes an
    /// unresolved edge exactly as in a full scan.
    internal bool TryResolveEndpoint(ISymbol symbol, out string endpoint)
    {
        // A constructed generic member maps to its definition.
        symbol = symbol.OriginalDefinition;
        for (int attempt = 0; attempt < 2; attempt++)
        {
            if (_symbolToId.TryGetValue(symbol, out var id))
            {
                endpoint = id;
                return true;
            }
            if (_filtered && _projectSymbols.Contains(symbol))
            {
                endpoint = CanonicalFqn(symbol);
                return true;
            }
            if (attempt == 1 || symbol.DeclaringSyntaxReferences.IsEmpty) break;

            // A symbol seen through another compilation (a project reference,
            // retargeted or not, or another project's copy of a shared file)
            // is a different instance: map it back by its declaration site —
            // text offsets, so independent of the compilation that parsed it.
            int known = _symbolToId.Count + _projectSymbols.Count;
            if (_declSitesCount != known)
            {
                _declSites.Clear();
                foreach (var s in _symbolToId.Keys.Concat(_projectSymbols))
                {
                    foreach (var r in s.DeclaringSyntaxReferences)
                        _declSites.TryAdd((r.SyntaxTree.FilePath, r.Span.Start, s.Kind, s.MetadataName), s);
                }
                _declSitesCount = known;
            }
            ISymbol? owner = null;
            foreach (var r in symbol.DeclaringSyntaxReferences)
            {
                if (_declSites.TryGetValue((r.SyntaxTree.FilePath, r.Span.Start, symbol.Kind, symbol.MetadataName), out owner)) break;
            }
            if (owner == null) break;
            symbol = owner;
        }
        endpoint = "";
        return false;
    }

    // Declaration-site index (path, start, kind, metadata name) -> the emitted
    // or registered symbol, rebuilt lazily when either set has grown.
    private readonly Dictionary<(string, int, SymbolKind, string), ISymbol> _declSites = new();
    private int _declSitesCount = -1;

    // Shared emission state the pass emitters read/write (the emitters hold
    // this Scanner as a collaborator; the maps stay here so id allocation and
    // the emitted sets remain single-sourced).
    internal Dictionary<ISymbol, string> SymbolToId => _symbolToId;
    internal Dictionary<string, string> DeclFqnToId => _declFqnToId;
    internal HashSet<string> EmittedModules => _emittedModules;
    internal HashSet<string> EmittedUnresolved => _emittedUnresolved;
}
