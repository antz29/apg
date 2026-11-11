using System;
using System.Collections.Generic;
using System.Collections.Immutable;
using System.IO;
using System.Linq;
using System.Text;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace Apg.CsharpFrontend;

/// <summary>
/// Roslyn compilation loading: parse trees, core BCL metadata references, the
/// <see cref="CSharpCompilation"/>, and one <see cref="SemanticModel"/> per
/// tree.
/// </summary>
internal static class CompilationLoader
{
    /// <summary>
    /// Parses every discovered file into a syntax tree (UTF-8) with
    /// <paramref name="options"/> — a project's preprocessor symbols and
    /// language version; null means latest language version, no symbols:
    /// .cs as regular C#, .csx as a script. .cshtml/.razor are never
    /// parsed as C#; their paths are returned through <paramref name="heldBack"/>
    /// in discovery order.
    /// </summary>
    public static List<SyntaxTree> ParseTrees(IEnumerable<string> files, out List<string> heldBack, CSharpParseOptions? options = null)
    {
        var syntaxTrees = new List<SyntaxTree>();
        heldBack = new List<string>();
        options ??= CSharpParseOptions.Default.WithLanguageVersion(LanguageVersion.Latest);
        foreach (var file in files)
        {
            if (file.EndsWith(".cshtml", StringComparison.OrdinalIgnoreCase)
                || file.EndsWith(".razor", StringComparison.OrdinalIgnoreCase))
            {
                heldBack.Add(file);
                continue;
            }
            var text = File.ReadAllText(file, Encoding.UTF8);
            var tree = CSharpSyntaxTree.ParseText(
                text,
                file.EndsWith(".csx", StringComparison.OrdinalIgnoreCase)
                    ? options.WithKind(SourceCodeKind.Script)
                    : options,
                path: file,
                encoding: Encoding.UTF8);
            syntaxTrees.Add(tree);
        }
        return syntaxTrees;
    }

    /// <summary>
    /// Builds a projectless compilation with core BCL metadata references
    /// only, over trees already parsed at the latest language version. It has
    /// exactly two uses: the single compilation over every .cs file outside
    /// every project (always the FULL such set: a target set filters
    /// emission, not resolution), and one compilation per .csx script over
    /// exactly that script's tree. A .csx tree never joins the .cs
    /// compilation, and no .cs tree joins a script's, so each script binds
    /// only its own declarations.
    /// </summary>
    public static CSharpCompilation CreateCompilation(List<SyntaxTree> syntaxTrees)
    {
        return CSharpCompilation.Create(
            "ApgScanAssembly",
            syntaxTrees,
            GetDefaultMetadataReferences(),
            new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, allowUnsafe: true));
    }

    /// <summary>
    /// Builds one project's compilation, named after the project, over its
    /// parsed trees with its framework references first, then its resolved
    /// package and assembly references, de-duplicated by reference identity,
    /// then a compilation reference to each already-built referenced
    /// project's compilation.
    /// </summary>
    public static CSharpCompilation CreateProjectCompilation(
        CsharpProject project, string scanRoot, List<SyntaxTree> syntaxTrees, IEnumerable<CSharpCompilation> referencedCompilations)
    {
        var references = new List<MetadataReference>();
        var seen = new HashSet<MetadataReference>();
        foreach (var reference in ReferenceResolver.ResolveFrameworkReferences(project).Concat(ReferenceResolver.Resolve(project, scanRoot)))
        {
            if (seen.Add(reference)) references.Add(reference);
        }
        foreach (var referenced in referencedCompilations)
        {
            references.Add(referenced.ToMetadataReference());
        }
        return CSharpCompilation.Create(
            project.Name,
            syntaxTrees,
            references,
            new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, allowUnsafe: true));
    }

    /// <summary>One semantic model per tree, for the whole project.</summary>
    public static List<(SyntaxTree Tree, SemanticModel Model, CompilationUnitSyntax Root)> BuildModels(
        CSharpCompilation compilation, List<SyntaxTree> syntaxTrees)
    {
        var unitModels = new List<(SyntaxTree Tree, SemanticModel Model, CompilationUnitSyntax Root)>();
        foreach (var tree in syntaxTrees)
        {
            var model = compilation.GetSemanticModel(tree);
            var root = tree.GetCompilationUnitRoot();
            unitModels.Add((tree, model, root));
        }
        return unitModels;
    }

    private const string CarriedRuntimePrefix = "apg.refs/";

    // Lazy rethrows the original exception, so an empty set surfaces as `csharpfrontend error:`.
    private static readonly Lazy<ImmutableArray<MetadataReference>> CarriedRuntime = new(LoadCarriedRuntime);

    /// <summary>
    /// The carried runtime: the net9.0 Microsoft.NETCore.App reference
    /// assemblies embedded in this assembly under <c>apg.refs/</c>, created
    /// once and shared read-only. Callers adding references copy first.
    /// </summary>
    internal static IReadOnlyList<MetadataReference> GetDefaultMetadataReferences() => CarriedRuntime.Value;

    private static ImmutableArray<MetadataReference> LoadCarriedRuntime()
    {
        var asm = typeof(CompilationLoader).Assembly;
        var refs = ImmutableArray.CreateBuilder<MetadataReference>();
        foreach (var name in asm.GetManifestResourceNames()
                     .Where(n => n.StartsWith(CarriedRuntimePrefix, StringComparison.Ordinal))
                     .OrderBy(n => n, StringComparer.Ordinal))
        {
            using var stream = asm.GetManifestResourceStream(name)!;
            refs.Add(MetadataReference.CreateFromStream(stream, filePath: name));
        }
        if (refs.Count == 0)
            throw new InvalidOperationException($"no carried-runtime reference assemblies embedded under `{CarriedRuntimePrefix}`");
        return refs.ToImmutable();
    }
}
