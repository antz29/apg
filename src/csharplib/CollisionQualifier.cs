using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace Apg.CsharpFrontend;

/// <summary>
/// Qualifies a top-level type whose namespace-based FQN several compilations
/// declare, so each project's copy gets its own identity under its project module.
/// Keeps the maps of the latest <see cref="Compute"/> call (the ones Scanner.Run
/// computes per scan) for <see cref="QualifiedParent"/>: the (declaring assembly,
/// plain FQN) key is authoritative, the (file path, plain FQN) key the fallback.
/// </summary>
internal static class CollisionQualifier
{
    private static Dictionary<(string File, string Type), string> byFile = new();
    // Assembly identity, never AssemblyName: names repeat (two App.csproj, every ApgScanAssembly).
    private static Dictionary<IAssemblySymbol, Dictionary<string, string>> byAssembly = new(SymbolEqualityComparer.Default);

    /// <summary>
    /// The unrooted qualified parent of a top-level collision-member type, or null
    /// (nested, metadata, .csx, projectless, single-compilation or unknown type).
    /// </summary>
    public static string? QualifiedParent(INamedTypeSymbol type)
    {
        type = type.OriginalDefinition;
        if (type.ContainingType != null || type.DeclaringSyntaxReferences.IsEmpty) return null;
        var file = type.DeclaringSyntaxReferences[0].SyntaxTree.FilePath;
        if (file.EndsWith(".csx", StringComparison.OrdinalIgnoreCase)) return null;
        var ns = type.ContainingNamespace is { IsGlobalNamespace: false } n ? n.ToDisplayString() : "";
        var name = SymbolNaming.GetTypeNameWithArity(type);
        var plain = ns.Length == 0 ? name : $"{ns}.{name}";
        if (type.ContainingAssembly is { } asm && byAssembly.TryGetValue(asm, out var types) && types.TryGetValue(plain, out var parent)) return parent;
        return byFile.TryGetValue((file, plain), out parent) ? parent : null;
    }

    /// <summary>
    /// Over every emitted unit — its semantic model and root from the one
    /// compilation it is emitted from, plus its owning project (null for the
    /// projectless compilation) — finds each top-level type whose plain FQN
    /// (`Namespace.Name`, arity-suffixed) is declared in more than one
    /// compilation and maps every declaring (file path, plain FQN) of a project
    /// member to its unrooted qualified parent: the project file's absolute
    /// path (task 5's module identity, whole) plus `.Namespace`, or the path
    /// alone for the global namespace. Projectless members, single-compilation
    /// types and .csx script types get no entry (they keep the plain FQN).
    /// Also replaces the stored file- and assembly-keyed maps QualifiedParent reads.
    /// </summary>
    public static Dictionary<(string File, string Type), string> Compute(
        IEnumerable<(SemanticModel Model, CompilationUnitSyntax Root, CsharpProject? Owner)> units)
    {
        // plain FQN -> compilation key ("" = projectless) -> (owner, declaring files)
        var decls = new Dictionary<string, Dictionary<string, (CsharpProject? Owner, HashSet<string> Files, HashSet<IAssemblySymbol> Assemblies)>>(StringComparer.Ordinal);
        var namespaces = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var (model, root, owner) in units)
        {
            var file = root.SyntaxTree.FilePath;
            if (file.EndsWith(".csx", StringComparison.OrdinalIgnoreCase)) continue;
            foreach (var typeDecl in root.DescendantNodes().OfType<BaseTypeDeclarationSyntax>())
            {
                // Top-level only; a script's types sit in its Script class and are skipped here too.
                if (model.GetDeclaredSymbol(typeDecl) is not INamedTypeSymbol { ContainingType: null } sym) continue;
                var ns = sym.ContainingNamespace is { IsGlobalNamespace: false } n ? n.ToDisplayString() : "";
                var name = SymbolNaming.GetTypeNameWithArity(sym);
                var plain = ns.Length == 0 ? name : $"{ns}.{name}";
                namespaces[plain] = ns;
                if (!decls.TryGetValue(plain, out var byCompilation)) decls[plain] = byCompilation = new(StringComparer.Ordinal);
                var key = owner?.Path ?? "";
                if (!byCompilation.TryGetValue(key, out var entry)) byCompilation[key] = entry = (owner, new HashSet<string>(StringComparer.Ordinal), new HashSet<IAssemblySymbol>(SymbolEqualityComparer.Default));
                entry.Files.Add(file);
                entry.Assemblies.Add(model.Compilation.Assembly);
            }
        }
        var qualified = new Dictionary<(string File, string Type), string>();
        var assemblies = new Dictionary<IAssemblySymbol, Dictionary<string, string>>(SymbolEqualityComparer.Default);
        foreach (var (plain, byCompilation) in decls)
        {
            if (byCompilation.Count < 2) continue;
            var ns = namespaces[plain];
            foreach (var (owner, files, asms) in byCompilation.Values)
            {
                if (owner == null) continue;
                var parent = ns.Length == 0 ? owner.Path : $"{owner.Path}.{ns}";
                foreach (var f in files) qualified[(f, plain)] = parent;
                foreach (var a in asms)
                {
                    if (!assemblies.TryGetValue(a, out var types)) assemblies[a] = types = new(StringComparer.Ordinal);
                    types[plain] = parent;
                }
            }
        }
        byFile = qualified;
        byAssembly = assemblies;
        return qualified;
    }
}
