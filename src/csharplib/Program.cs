using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Text;

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

// --- Unified JSONL Schema records (SPEC §2) ---

public class ModuleMsg
{
    [JsonPropertyName("type")]
    public string Type => "module";

    [JsonPropertyName("fqn")]
    public string Fqn { get; set; } = "";
}

public class FileMsg
{
    [JsonPropertyName("type")]
    public string Type => "file";

    [JsonPropertyName("path")]
    public string Path { get; set; } = "";

    [JsonPropertyName("parent")]
    public string Parent { get; set; } = "";

    [JsonPropertyName("start_line")]
    public int StartLine { get; set; }

    [JsonPropertyName("end_line")]
    public int EndLine { get; set; }
}

public class StructMsg
{
    [JsonPropertyName("type")]
    public string Type => "struct";

    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    [JsonPropertyName("parent")]
    public string Parent { get; set; } = "";

    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    [JsonPropertyName("path")]
    public string Path { get; set; } = "";

    [JsonPropertyName("start")]
    public int Start { get; set; }

    [JsonPropertyName("end")]
    public int End { get; set; }

    [JsonPropertyName("start_line")]
    public int StartLine { get; set; }

    [JsonPropertyName("end_line")]
    public int EndLine { get; set; }
}

public class FuncMsg
{
    [JsonPropertyName("type")]
    public string Type => "function";

    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    [JsonPropertyName("parent")]
    public string Parent { get; set; } = "";

    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    [JsonPropertyName("params")]
    public List<string> Params { get; set; } = new();

    [JsonPropertyName("file")]
    public string File { get; set; } = "";

    [JsonPropertyName("path")]
    public string Path { get; set; } = "";

    [JsonPropertyName("start")]
    public int Start { get; set; }

    [JsonPropertyName("end")]
    public int End { get; set; }

    [JsonPropertyName("start_line")]
    public int StartLine { get; set; }

    [JsonPropertyName("end_line")]
    public int EndLine { get; set; }
}

public class UnresolvedMsg
{
    [JsonPropertyName("type")]
    public string Type => "unresolved";

    [JsonPropertyName("fqn")]
    public string Fqn { get; set; } = "";

    [JsonPropertyName("category")]
    public string Category { get; set; } = "";
}

public class EdgeMsg
{
    [JsonPropertyName("type")]
    public string Type { get; set; } = ""; // contains | calls | uses | unresolved_call | unresolved_use

    [JsonPropertyName("from")]
    public string From { get; set; } = "";

    [JsonPropertyName("to")]
    public string To { get; set; } = "";

    [JsonPropertyName("target_type")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? TargetType { get; set; }
}

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

    private string NextId()
    {
        lock (_lock)
        {
            _nodeCounter++;
            return $"{_idPrefix}{_nodeCounter}";
        }
    }

    public void Run()
    {
        var files = DiscoverFiles(_rootDir).ToList();
        if (files.Count == 0)
        {
            Console.Error.WriteLine("No .cs files found to scan.");
            return;
        }

        Console.Error.WriteLine($"Scanning {files.Count} C# source files...");

        // Parse all syntax trees
        var syntaxTrees = new List<SyntaxTree>();
        foreach (var file in files)
        {
            var text = File.ReadAllText(file, Encoding.UTF8);
            var tree = CSharpSyntaxTree.ParseText(
                text,
                CSharpParseOptions.Default.WithLanguageVersion(LanguageVersion.Latest),
                path: file,
                encoding: Encoding.UTF8);
            syntaxTrees.Add(tree);
        }

        // Build compilation with core BCL metadata references. The compilation
        // always covers the FULL file set: the target set filters emission, not
        // resolution, so a target file's references resolve exactly.
        var references = GetDefaultMetadataReferences();
        var compilation = CSharpCompilation.Create(
            "ApgScanAssembly",
            syntaxTrees,
            references,
            new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, allowUnsafe: true));

        // An all-targets request (every discovered file selected) is the full
        // path, so it stays byte-identical to a plain full scan.
        bool filtered = _filtered && !files.All(f => _targetFiles.Contains(f));

        // One semantic model per tree, for the whole project.
        var unitModels = new List<(SyntaxTree Tree, SemanticModel Model, CompilationUnitSyntax Root)>();
        foreach (var tree in syntaxTrees)
        {
            var model = compilation.GetSemanticModel(tree);
            var root = tree.GetCompilationUnitRoot();
            unitModels.Add((tree, model, root));
        }

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
        foreach (var (tree, model, root) in unitModels)
        {
            EmitFileAndDeclarations(tree, model, root, emit: !filtered || IsTargetFile(tree.FilePath));
        }

        // Pass 2: Semantic walk for edges (calls & uses) — target files only
        // when a filter is in force.
        foreach (var (tree, model, root) in unitModels)
        {
            if (!filtered || IsTargetFile(tree.FilePath))
            {
                EmitEdges(tree, model, root);
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
        string key = MethodGroupKey(method);
        _methodGroups[key] = _methodGroups.TryGetValue(key, out var count) ? count + 1 : 1;
    }

    private static string MethodGroupKey(IMethodSymbol method)
        => GetParentScopeFqn(method) + "\u0000" + EmittedMethodName(method);

    /// The `name` the frontend emits for a method-like symbol: a constructor
    /// carries its containing type's simple name, a property accessor
    /// `get_`/`set_` + the property's name (an indexer's property name is
    /// `this[]`), everything else its own (arity-suffixed) name.
    private static string EmittedMethodName(IMethodSymbol method)
    {
        if (method.MethodKind == MethodKind.Constructor)
        {
            return method.ContainingType?.Name ?? method.Name;
        }
        if (method.MethodKind is MethodKind.PropertyGet or MethodKind.PropertySet
            && method.AssociatedSymbol is IPropertySymbol prop)
        {
            string prefix = method.MethodKind == MethodKind.PropertyGet ? "get_" : "set_";
            return prefix + prop.Name;
        }
        return GetMethodNameWithArity(method);
    }

    /// The canonical FQN the ingestor renders for a symbol declared in this
    /// project, computed from the same `parent`/`name`/`params` the frontend
    /// emits (SPEC §4 overload rendering).
    private string CanonicalFqn(ISymbol symbol)
    {
        if (symbol is IMethodSymbol method)
        {
            string parent = GetParentScopeFqn(method);
            string name = EmittedMethodName(method);
            string fqn = $"{parent}.{name}";
            if (_methodGroups.TryGetValue(MethodGroupKey(method), out var group) && group > 1)
            {
                var ps = method.Parameters.Select(GetParameterTypeString);
                fqn += $"({string.Join(",", ps)})";
            }
            return fqn;
        }
        // A type renders `parent.name`, where `parent` is exactly what the
        // emitted struct record carries (the containing type's FQN, or the
        // namespace).
        return $"{GetParentScopeFqn(symbol)}.{GetTypeNameWithArity((INamedTypeSymbol)symbol)}";
    }

    /// Resolves an edge endpoint: the opaque id when the target symbol is part
    /// of the emitted (target) set, else the canonical FQN when it is project
    /// code declared in a non-emitted file (the cached unit supplies its node).
    /// Returns false for a foreign symbol (external/stdlib), which becomes an
    /// unresolved edge exactly as in a full scan.
    private bool TryResolveEndpoint(ISymbol symbol, out string endpoint)
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

    private IEnumerable<string> DiscoverFiles(string root)
    {
        var stack = new Stack<string>();
        stack.Push(root);

        while (stack.Count > 0)
        {
            var current = stack.Pop();
            string[] subDirs;
            string[] files;

            try
            {
                subDirs = Directory.GetDirectories(current);
                files = Directory.GetFiles(current, "*.cs");
            }
            catch
            {
                continue;
            }

            foreach (var file in files)
            {
                var fullPath = Path.GetFullPath(file);
                if (IsExcluded(fullPath)) continue;
                if (_moduleDirs.Count > 0 && !_moduleDirs.Any(m => fullPath.StartsWith(m, StringComparison.Ordinal))) continue;
                yield return fullPath;
            }

            foreach (var subDir in subDirs)
            {
                var name = Path.GetFileName(subDir);
                if (name.StartsWith('.') || name == "bin" || name == "obj" || name == "node_modules")
                    continue;
                stack.Push(subDir);
            }
        }
    }

    private bool IsExcluded(string path)
    {
        foreach (var pat in _excludePatterns)
        {
            if (path.Contains(pat, StringComparison.OrdinalIgnoreCase))
                return true;
        }
        return false;
    }

    private static List<MetadataReference> GetDefaultMetadataReferences()
    {
        var refs = new List<MetadataReference>();
        
        // When running as single-file or standard, try trusted platform assemblies first,
        // then fall back to AppDomain / AppContext.BaseDirectory / runtime directory.
        var tpa = AppContext.GetData("TRUSTED_PLATFORM_ASSEMBLIES") as string;
        if (!string.IsNullOrEmpty(tpa))
        {
            foreach (var path in tpa.Split(Path.PathSeparator))
            {
                if (string.IsNullOrEmpty(path) || !File.Exists(path)) continue;
                try
                {
                    refs.Add(MetadataReference.CreateFromFile(path));
                }
                catch
                {
                    // Skip unreadable
                }
            }
        }

        if (refs.Count == 0)
        {
            var baseDir = AppContext.BaseDirectory;
            if (Directory.Exists(baseDir))
            {
                foreach (var dll in Directory.GetFiles(baseDir, "*.dll"))
                {
                    try
                    {
                        refs.Add(MetadataReference.CreateFromFile(dll));
                    }
                    catch
                    {
                        // Skip
                    }
                }
            }
        }

        // Also reference loaded assemblies where possible
        foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
        {
            try
            {
                if (!asm.IsDynamic && !string.IsNullOrEmpty(asm.Location) && File.Exists(asm.Location))
                {
                    refs.Add(MetadataReference.CreateFromFile(asm.Location));
                }
            }
            catch
            {
                // Skip
            }
        }

        return refs;
    }

    private void EmitJson(object obj)
    {
        var json = JsonSerializer.Serialize(obj);
        Console.WriteLine(json);
    }

    private void EmitFileAndDeclarations(SyntaxTree tree, SemanticModel model, CompilationUnitSyntax root, bool emit)
    {
        var filePath = tree.FilePath;
        var text = tree.GetText();
        var lines = text.Lines;
        int totalLines = Math.Max(1, lines.Count);

        // Determine parent module / namespace for file
        string defaultNamespace = "";
        var firstNs = root.DescendantNodes().OfType<BaseNamespaceDeclarationSyntax>().FirstOrDefault();
        if (firstNs != null)
        {
            defaultNamespace = firstNs.Name.ToString();
        }

        // Module scaffolding is global (it is not part of a per-file fact unit),
        // so it is emitted for every file — target or not — and the cached splice
        // sees the full Module→Module hierarchy.
        if (!string.IsNullOrEmpty(defaultNamespace))
        {
            EmitModuleHierarchy(defaultNamespace);
        }

        // Emit File Node
        if (emit)
        {
            EmitJson(new FileMsg
            {
                Path = filePath,
                Parent = defaultNamespace,
                StartLine = 1,
                EndLine = totalLines
            });
        }

        // Traverse declarations in file
        foreach (var member in root.DescendantNodes())
        {
            if (member is BaseTypeDeclarationSyntax typeDecl)
            {
                var symbol = model.GetDeclaredSymbol(typeDecl);
                if (symbol == null) continue;

                string parentFqn = GetParentScopeFqn(symbol);
                if (!string.IsNullOrEmpty(parentFqn))
                {
                    EmitModuleHierarchy(parentFqn);
                }

                // Handle partial classes / types: if symbol already has an assigned ID, reuse it
                // and avoid emitting duplicate Struct node (which causes FQN collision).
                if (!_symbolToId.TryGetValue(symbol, out var id))
                {
                    // A non-target file registers no id: its declarations are
                    // served from the shared fact cache, and its cross-file
                    // endpoints are emitted by canonical FQN.
                    if (!emit) continue;

                    id = NextId();
                    _symbolToId[symbol] = id;

                    var span = typeDecl.Span;
                    var lineSpan = tree.GetLineSpan(span);

                    string name = GetTypeNameWithArity(symbol);
                    string fullFqn = string.IsNullOrEmpty(parentFqn) ? name : $"{parentFqn}.{name}";
                    _declFqnToId[fullFqn] = id;

                    EmitJson(new StructMsg
                    {
                        Id = id,
                        Parent = parentFqn,
                        Name = name,
                        Path = filePath,
                        Start = span.Start,
                        End = span.End,
                        StartLine = lineSpan.StartLinePosition.Line + 1,
                        EndLine = lineSpan.EndLinePosition.Line + 1
                    });

                    // If nested type, emit Struct -> Struct Contains edge
                    if (symbol.ContainingType != null && TryResolveEndpoint(symbol.ContainingType, out var parentStructEndpoint))
                    {
                        EmitJson(new EdgeMsg
                        {
                            Type = "contains",
                            From = parentStructEndpoint,
                            To = id
                        });
                    }
                }
            }
            else if (emit && member is MethodDeclarationSyntax methodDecl)
            {
                EmitMethod(methodDecl, model, tree, filePath);
            }
            else if (emit && member is ConstructorDeclarationSyntax ctorDecl)
            {
                EmitConstructor(ctorDecl, model, tree, filePath);
            }
            else if (emit && member is PropertyDeclarationSyntax propDecl)
            {
                EmitPropertyAccessors(propDecl, model, tree, filePath);
            }
        }
    }

    private void EmitMethod(MethodDeclarationSyntax methodDecl, SemanticModel model, SyntaxTree tree, string filePath)
    {
        var symbol = model.GetDeclaredSymbol(methodDecl);
        if (symbol == null) return;

        var id = NextId();
        _symbolToId[symbol] = id;

        var span = methodDecl.Span;
        var lineSpan = tree.GetLineSpan(span);
        string parentFqn = GetParentScopeFqn(symbol);

        var paramTypes = symbol.Parameters.Select(GetParameterTypeString).ToList();

        string name = GetMethodNameWithArity(symbol);
        string funcKey = $"{parentFqn}.{name}";
        _declFqnToId[funcKey] = id;

        EmitJson(new FuncMsg
        {
            Id = id,
            Parent = parentFqn,
            Name = name,
            Params = paramTypes,
            File = filePath,
            Path = filePath,
            Start = span.Start,
            End = span.End,
            StartLine = lineSpan.StartLinePosition.Line + 1,
            EndLine = lineSpan.EndLinePosition.Line + 1
        });

        // If enclosed in a type, emit Struct -> Function Contains edge
        if (symbol.ContainingType != null && TryResolveEndpoint(symbol.ContainingType, out var parentStructEndpoint))
        {
            EmitJson(new EdgeMsg
            {
                Type = "contains",
                From = parentStructEndpoint,
                To = id
            });
        }
    }

    private void EmitConstructor(ConstructorDeclarationSyntax ctorDecl, SemanticModel model, SyntaxTree tree, string filePath)
    {
        var symbol = model.GetDeclaredSymbol(ctorDecl);
        if (symbol == null) return;

        var id = NextId();
        _symbolToId[symbol] = id;

        var span = ctorDecl.Span;
        var lineSpan = tree.GetLineSpan(span);
        string parentFqn = GetParentScopeFqn(symbol);

        var paramTypes = symbol.Parameters.Select(GetParameterTypeString).ToList();
        string name = symbol.ContainingType?.Name ?? symbol.Name;
        string funcKey = $"{parentFqn}.{name}";
        _declFqnToId[funcKey] = id;

        EmitJson(new FuncMsg
        {
            Id = id,
            Parent = parentFqn,
            Name = name,
            Params = paramTypes,
            File = filePath,
            Path = filePath,
            Start = span.Start,
            End = span.End,
            StartLine = lineSpan.StartLinePosition.Line + 1,
            EndLine = lineSpan.EndLinePosition.Line + 1
        });

        if (symbol.ContainingType != null && TryResolveEndpoint(symbol.ContainingType, out var parentStructEndpoint))
        {
            EmitJson(new EdgeMsg
            {
                Type = "contains",
                From = parentStructEndpoint,
                To = id
            });
        }
    }

    private void EmitPropertyAccessors(PropertyDeclarationSyntax propDecl, SemanticModel model, SyntaxTree tree, string filePath)
    {
        var propSymbol = model.GetDeclaredSymbol(propDecl);
        if (propSymbol == null) return;

        if (propSymbol.GetMethod != null)
        {
            EmitAccessor(propSymbol.GetMethod, propDecl, tree, filePath, $"get_{propSymbol.Name}");
        }
        if (propSymbol.SetMethod != null)
        {
            EmitAccessor(propSymbol.SetMethod, propDecl, tree, filePath, $"set_{propSymbol.Name}");
        }
    }

    private void EmitAccessor(IMethodSymbol symbol, PropertyDeclarationSyntax propDecl, SyntaxTree tree, string filePath, string name)
    {
        var id = NextId();
        _symbolToId[symbol] = id;

        var span = propDecl.Span;
        var lineSpan = tree.GetLineSpan(span);
        string parentFqn = GetParentScopeFqn(symbol);

        var paramTypes = symbol.Parameters.Select(GetParameterTypeString).ToList();
        string funcKey = $"{parentFqn}.{name}";
        _declFqnToId[funcKey] = id;

        EmitJson(new FuncMsg
        {
            Id = id,
            Parent = parentFqn,
            Name = name,
            Params = paramTypes,
            File = filePath,
            Path = filePath,
            Start = span.Start,
            End = span.End,
            StartLine = lineSpan.StartLinePosition.Line + 1,
            EndLine = lineSpan.EndLinePosition.Line + 1
        });

        if (symbol.ContainingType != null && TryResolveEndpoint(symbol.ContainingType, out var parentStructEndpoint))
        {
            EmitJson(new EdgeMsg
            {
                Type = "contains",
                From = parentStructEndpoint,
                To = id
            });
        }
    }

    private void EmitModuleHierarchy(string fqn)
    {
        if (string.IsNullOrEmpty(fqn)) return;
        var parts = fqn.Split('.');
        string current = "";

        for (int i = 0; i < parts.Length; i++)
        {
            string prev = current;
            current = i == 0 ? parts[0] : $"{current}.{parts[i]}";

            if (_emittedModules.Add(current))
            {
                EmitJson(new ModuleMsg { Fqn = current });
                if (!string.IsNullOrEmpty(prev))
                {
                    EmitJson(new EdgeMsg
                    {
                        Type = "contains",
                        From = prev,
                        To = current
                    });
                }
            }
        }
    }

    private void EmitEdges(SyntaxTree tree, SemanticModel model, CompilationUnitSyntax root)
    {
        foreach (var node in root.DescendantNodes())
        {
            // Resolve Calls from Invocation Expressions
            if (node is InvocationExpressionSyntax invocation)
            {
                var callerSymbol = GetEnclosingExecutableSymbol(node, model);
                if (callerSymbol != null && _symbolToId.TryGetValue(callerSymbol, out var callerId))
                {
                    var symbolInfo = model.GetSymbolInfo(invocation);
                    var targetSymbol = symbolInfo.Symbol ?? symbolInfo.CandidateSymbols.FirstOrDefault();

                    if (targetSymbol is IMethodSymbol targetMethod)
                    {
                        if (TryResolveEndpoint(targetMethod, out var targetEndpoint))
                        {
                            EmitJson(new EdgeMsg
                            {
                                Type = "calls",
                                From = callerId,
                                To = targetEndpoint
                            });
                        }
                        else
                        {
                            EmitUnresolvedCall(callerId, targetMethod);
                        }
                    }
                    else if (targetSymbol is IFieldSymbol or ILocalSymbol or IParameterSymbol)
                    {
                        // Delegate call / Func-value call
                        EmitJson(new EdgeMsg
                        {
                            Type = "unresolved_call",
                            From = callerId,
                            To = invocation.Expression.ToString(),
                            TargetType = targetSymbol.ToString()
                        });
                    }
                }
            }
            // Resolve Calls from Object Creations (constructors)
            else if (node is ObjectCreationExpressionSyntax creation)
            {
                var callerSymbol = GetEnclosingExecutableSymbol(node, model);
                if (callerSymbol != null && _symbolToId.TryGetValue(callerSymbol, out var callerId))
                {
                    var symbolInfo = model.GetSymbolInfo(creation);
                    var targetSymbol = symbolInfo.Symbol ?? symbolInfo.CandidateSymbols.FirstOrDefault();

                    if (targetSymbol is IMethodSymbol targetCtor)
                    {
                        if (TryResolveEndpoint(targetCtor, out var targetEndpoint))
                        {
                            EmitJson(new EdgeMsg
                            {
                                Type = "calls",
                                From = callerId,
                                To = targetEndpoint
                            });
                        }
                        else
                        {
                            EmitUnresolvedCall(callerId, targetCtor);
                        }
                    }
                    else
                    {
                        // Fall back to Uses edge to created type
                        var typeInfo = model.GetTypeInfo(creation);
                        if (typeInfo.Type is INamedTypeSymbol createdType)
                        {
                            EmitTypeUse(callerId, createdType);
                        }
                    }
                }
            }
            // Resolve Uses from Type Declarations (Base Types / Interfaces)
            else if (node is BaseTypeDeclarationSyntax baseTypeDecl)
            {
                var declaredType = model.GetDeclaredSymbol(baseTypeDecl);
                if (declaredType != null && _symbolToId.TryGetValue(declaredType, out var structId))
                {
                    if (declaredType.BaseType != null && declaredType.BaseType.SpecialType != SpecialType.System_Object)
                    {
                        EmitTypeUse(structId, declaredType.BaseType);
                    }
                    foreach (var iface in declaredType.Interfaces)
                    {
                        EmitTypeUse(structId, iface);
                    }
                }
            }
            // Resolve Uses from Type Syntax (Fields, Properties, Local variables, Casts)
            else if (node is TypeSyntax typeSyntax && node.Parent is not (BaseTypeDeclarationSyntax or NamespaceDeclarationSyntax or FileScopedNamespaceDeclarationSyntax))
            {
                var enclosingSymbol = GetEnclosingExecutableSymbol(node, model) ?? GetEnclosingTypeSymbol(node, model);
                if (enclosingSymbol != null && _symbolToId.TryGetValue(enclosingSymbol, out var fromId))
                {
                    var typeInfo = model.GetTypeInfo(typeSyntax);
                    if (typeInfo.Type is INamedTypeSymbol usedType)
                    {
                        EmitTypeUse(fromId, usedType);
                    }
                }
            }
        }
    }

    private void EmitTypeUse(string fromId, INamedTypeSymbol targetType)
    {
        if (targetType.SpecialType != SpecialType.None && targetType.SpecialType != SpecialType.System_Object)
            return;

        if (TryResolveEndpoint(targetType, out var targetEndpoint))
        {
            EmitJson(new EdgeMsg
            {
                Type = "uses",
                From = fromId,
                To = targetEndpoint
            });
        }
        else
        {
            string fqn = GetFullFqn(targetType);
            if (string.IsNullOrEmpty(fqn)) return;

            EmitUnresolvedNode(fqn, ClassifyCategory(targetType));
            EmitJson(new EdgeMsg
            {
                Type = "unresolved_use",
                From = fromId,
                To = fqn
            });
        }
    }

    private void EmitUnresolvedCall(string fromId, IMethodSymbol targetMethod)
    {
        string parentFqn = GetParentScopeFqn(targetMethod);
        string methodName = GetMethodNameWithArity(targetMethod);
        string fqn = string.IsNullOrEmpty(parentFqn) ? methodName : $"{parentFqn}.{methodName}";
        string category = ClassifyCategory(targetMethod.ContainingType);

        EmitUnresolvedNode(fqn, category);
        EmitJson(new EdgeMsg
        {
            Type = "unresolved_call",
            From = fromId,
            To = fqn
        });
    }

    private void EmitUnresolvedNode(string fqn, string category)
    {
        if (_emittedUnresolved.Add(fqn))
        {
            EmitJson(new UnresolvedMsg
            {
                Fqn = fqn,
                Category = category
            });
        }
    }

    private static string ClassifyCategory(ITypeSymbol? type)
    {
        if (type == null) return "unknown";
        var ns = type.ContainingNamespace?.ToDisplayString() ?? "";
        if (ns.StartsWith("System", StringComparison.Ordinal) || ns.StartsWith("Microsoft", StringComparison.Ordinal))
        {
            return "stdlib";
        }
        return "external";
    }

    private static ISymbol? GetEnclosingExecutableSymbol(SyntaxNode node, SemanticModel model)
    {
        var current = node.Parent;
        while (current != null)
        {
            if (current is MethodDeclarationSyntax or ConstructorDeclarationSyntax or AccessorDeclarationSyntax or LocalFunctionStatementSyntax)
            {
                return model.GetDeclaredSymbol(current);
            }
            current = current.Parent;
        }
        return null;
    }

    private static ISymbol? GetEnclosingTypeSymbol(SyntaxNode node, SemanticModel model)
    {
        var current = node.Parent;
        while (current != null)
        {
            if (current is BaseTypeDeclarationSyntax typeDecl)
            {
                return model.GetDeclaredSymbol(typeDecl);
            }
            current = current.Parent;
        }
        return null;
    }

    private static string GetParentScopeFqn(ISymbol symbol)
    {
        if (symbol.ContainingType != null)
        {
            return GetFullFqn(symbol.ContainingType);
        }
        return symbol.ContainingNamespace is { IsGlobalNamespace: false } ns ? ns.ToDisplayString() : "";
    }

    private static string GetTypeNameWithArity(INamedTypeSymbol symbol)
    {
        if (symbol.Arity > 0)
        {
            return $"{symbol.Name}`{symbol.Arity}";
        }
        return symbol.Name;
    }

    private static string GetMethodNameWithArity(IMethodSymbol symbol)
    {
        if (symbol.Arity > 0)
        {
            return $"{symbol.Name}`{symbol.Arity}";
        }
        return symbol.Name;
    }

    private static string GetFullFqn(ISymbol symbol)
    {
        if (symbol is INamespaceSymbol ns)
        {
            return ns.IsGlobalNamespace ? "" : ns.ToDisplayString();
        }
        if (symbol is INamedTypeSymbol namedType)
        {
            string typeName = GetTypeNameWithArity(namedType);
            if (symbol.ContainingType != null)
            {
                var parent = GetFullFqn(symbol.ContainingType);
                return string.IsNullOrEmpty(parent) ? typeName : $"{parent}.{typeName}";
            }
            if (symbol.ContainingNamespace is { IsGlobalNamespace: false } ns1)
            {
                return $"{ns1.ToDisplayString()}.{typeName}";
            }
            return typeName;
        }
        if (symbol.ContainingType != null)
        {
            var parent = GetFullFqn(symbol.ContainingType);
            return string.IsNullOrEmpty(parent) ? symbol.Name : $"{parent}.{symbol.Name}";
        }
        if (symbol.ContainingNamespace is { IsGlobalNamespace: false } ns2)
        {
            return $"{ns2.ToDisplayString()}.{symbol.Name}";
        }
        return symbol.Name;
    }

    private static string GetParameterTypeString(IParameterSymbol param)
    {
        var prefix = param.RefKind switch
        {
            RefKind.Out => "out ",
            RefKind.Ref => "ref ",
            RefKind.In or RefKind.RefReadOnlyParameter => "in ",
            _ => ""
        };
        return prefix + param.Type.ToDisplayString(SymbolDisplayFormat.MinimallyQualifiedFormat);
    }
}
