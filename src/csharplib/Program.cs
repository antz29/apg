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
            Console.Error.WriteLine("Usage: csharpfrontend <project_dir> [--module <dir>]... [--id-prefix <p>] [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude_globs...]");
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
            else
            {
                excludePatterns.Add(args[i]);
            }
            i++;
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
        var files = new SourceDiscovery(_moduleDirs, _excludePatterns).DiscoverFiles(_rootDir).ToList();
        if (files.Count == 0)
        {
            Console.Error.WriteLine("No .cs files found to scan.");
            return;
        }

        Console.Error.WriteLine($"Scanning {files.Count} C# source files...");

        // Parse all syntax trees, then build a compilation with core BCL
        // metadata references. The compilation always covers the FULL file
        // set: the target set filters emission, not resolution, so a target
        // file's references resolve exactly.
        var syntaxTrees = CompilationLoader.ParseTrees(files);
        var compilation = CompilationLoader.CreateCompilation(syntaxTrees);

        // An all-targets request (every discovered file selected) is the full
        // path, so it stays byte-identical to a plain full scan.
        bool filtered = _filtered && !files.All(f => _targetFiles.Contains(f));

        // One semantic model per tree, for the whole project.
        var unitModels = CompilationLoader.BuildModels(compilation, syntaxTrees);

        if (filtered)
        {
            // Register the whole project's declaration universe and overload
            // groups before emitting, so a target file's edge to a symbol
            // declared in a non-emitted file lands on that symbol's canonical
            // FQN (the cached unit supplies the node).
            Console.Error.WriteLine($"Target set in force: emitting facts for {files.Count(f => IsTargetFile(f))} of {files.Count} files.");
            RegisterUniverse(unitModels);
        }

        // Pass 1: Collect declarations & emit nodes. Module scaffolding is
        // global (it is not part of a per-file fact unit), so a non-target file
        // still contributes its module hierarchy; only its declarations and
        // edges are withheld.
        var declarations = new DeclarationEmitter(this);
        foreach (var (tree, model, root) in unitModels)
        {
            declarations.EmitFileAndDeclarations(tree, model, root, emit: !filtered || IsTargetFile(tree.FilePath));
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

    /// Registration prepass over the full project: records every declared
    /// project symbol and counts each method-like declaration's
    /// `(parent,name)` group so the canonical FQN can carry the ingestor's
    /// overload suffix.
    private void RegisterUniverse(List<(SyntaxTree Tree, SemanticModel Model, CompilationUnitSyntax Root)> units)
    {
        foreach (var (_, model, root) in units)
        {
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
        // emitted struct record carries (the containing type's FQN, or the
        // namespace).
        return $"{SymbolNaming.GetParentScopeFqn(symbol)}.{SymbolNaming.GetTypeNameWithArity((INamedTypeSymbol)symbol)}";
    }

    /// Resolves an edge endpoint: the opaque id when the target symbol is part
    /// of the emitted (target) set, else the canonical FQN when it is project
    /// code declared in a non-emitted file (the cached unit supplies its node).
    /// Returns false for a foreign symbol (external/stdlib), which becomes an
    /// unresolved edge exactly as in a full scan.
    internal bool TryResolveEndpoint(ISymbol symbol, out string endpoint)
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
        endpoint = "";
        return false;
    }

    // Shared emission state the pass emitters read/write (the emitters hold
    // this Scanner as a collaborator; the maps stay here so id allocation and
    // the emitted sets remain single-sourced).
    internal Dictionary<ISymbol, string> SymbolToId => _symbolToId;
    internal Dictionary<string, string> DeclFqnToId => _declFqnToId;
    internal HashSet<string> EmittedModules => _emittedModules;
    internal HashSet<string> EmittedUnresolved => _emittedUnresolved;
}
