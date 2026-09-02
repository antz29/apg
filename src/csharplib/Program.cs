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
            Console.Error.WriteLine("Usage: csharpfrontend <project_dir> [--module <dir>]... [exclude_globs...]");
            return 1;
        }

        string rootDir = Path.GetFullPath(args[0]);
        var moduleDirs = new List<string>();
        var excludePatterns = new List<string>();

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
            else
            {
                excludePatterns.Add(args[i]);
            }
            i++;
        }

        try
        {
            var scanner = new Scanner(rootDir, moduleDirs, excludePatterns);
            scanner.Run();
            return 0;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"csharpfrontend error: {ex}");
            return 1;
        }
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

    private int _nodeCounter = 0;
    private readonly object _lock = new();

    private readonly Dictionary<ISymbol, string> _symbolToId = new(SymbolEqualityComparer.Default);
    private readonly Dictionary<string, string> _declFqnToId = new();
    private readonly HashSet<string> _emittedModules = new();
    private readonly HashSet<string> _emittedUnresolved = new();

    public Scanner(string rootDir, List<string> moduleDirs, List<string> excludePatterns)
    {
        _rootDir = rootDir;
        _moduleDirs = moduleDirs;
        _excludePatterns = excludePatterns;
    }

    private string NextId()
    {
        lock (_lock)
        {
            _nodeCounter++;
            return $"n{_nodeCounter}";
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

        // Build compilation with core BCL metadata references
        var references = GetDefaultMetadataReferences();
        var compilation = CSharpCompilation.Create(
            "ApgScanAssembly",
            syntaxTrees,
            references,
            new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, allowUnsafe: true));

        // Pass 1: Collect declarations & emit nodes
        var unitModels = new List<(SyntaxTree Tree, SemanticModel Model, CompilationUnitSyntax Root)>();
        foreach (var tree in syntaxTrees)
        {
            var model = compilation.GetSemanticModel(tree);
            var root = tree.GetCompilationUnitRoot();
            unitModels.Add((tree, model, root));

            EmitFileAndDeclarations(tree, model, root);
        }

        // Pass 2: Semantic walk for edges (calls & uses)
        foreach (var (tree, model, root) in unitModels)
        {
            EmitEdges(tree, model, root);
        }
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

    private void EmitFileAndDeclarations(SyntaxTree tree, SemanticModel model, CompilationUnitSyntax root)
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

        if (!string.IsNullOrEmpty(defaultNamespace))
        {
            EmitModuleHierarchy(defaultNamespace);
        }

        // Emit File Node
        EmitJson(new FileMsg
        {
            Path = filePath,
            Parent = defaultNamespace,
            StartLine = 1,
            EndLine = totalLines
        });

        // Traverse declarations in file
        foreach (var member in root.DescendantNodes())
        {
            if (member is BaseTypeDeclarationSyntax typeDecl)
            {
                var symbol = model.GetDeclaredSymbol(typeDecl);
                if (symbol == null) continue;

                // Handle partial classes / types: if symbol already has an assigned ID, reuse it
                // and avoid emitting duplicate Struct node (which causes FQN collision).
                if (!_symbolToId.TryGetValue(symbol, out var id))
                {
                    id = NextId();
                    _symbolToId[symbol] = id;

                    var span = typeDecl.Span;
                    var lineSpan = tree.GetLineSpan(span);

                    string parentFqn = GetParentScopeFqn(symbol);
                    if (!string.IsNullOrEmpty(parentFqn))
                    {
                        EmitModuleHierarchy(parentFqn);
                    }

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
                    if (symbol.ContainingType != null && _symbolToId.TryGetValue(symbol.ContainingType, out var parentStructId))
                    {
                        EmitJson(new EdgeMsg
                        {
                            Type = "contains",
                            From = parentStructId,
                            To = id
                        });
                    }
                }
            }
            else if (member is MethodDeclarationSyntax methodDecl)
            {
                EmitMethod(methodDecl, model, tree, filePath);
            }
            else if (member is ConstructorDeclarationSyntax ctorDecl)
            {
                EmitConstructor(ctorDecl, model, tree, filePath);
            }
            else if (member is PropertyDeclarationSyntax propDecl)
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
        if (symbol.ContainingType != null && _symbolToId.TryGetValue(symbol.ContainingType, out var parentStructId))
        {
            EmitJson(new EdgeMsg
            {
                Type = "contains",
                From = parentStructId,
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

        if (symbol.ContainingType != null && _symbolToId.TryGetValue(symbol.ContainingType, out var parentStructId))
        {
            EmitJson(new EdgeMsg
            {
                Type = "contains",
                From = parentStructId,
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

        if (symbol.ContainingType != null && _symbolToId.TryGetValue(symbol.ContainingType, out var parentStructId))
        {
            EmitJson(new EdgeMsg
            {
                Type = "contains",
                From = parentStructId,
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
                        if (_symbolToId.TryGetValue(targetMethod, out var targetId))
                        {
                            EmitJson(new EdgeMsg
                            {
                                Type = "calls",
                                From = callerId,
                                To = targetId
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
                        if (_symbolToId.TryGetValue(targetCtor, out var targetId))
                        {
                            EmitJson(new EdgeMsg
                            {
                                Type = "calls",
                                From = callerId,
                                To = targetId
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

        if (_symbolToId.TryGetValue(targetType, out var targetStructId))
        {
            EmitJson(new EdgeMsg
            {
                Type = "uses",
                From = fromId,
                To = targetStructId
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
