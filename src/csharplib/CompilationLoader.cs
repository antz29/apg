using System;
using System.Collections.Generic;
using System.IO;
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
        var references = GetDefaultMetadataReferences();
        return CSharpCompilation.Create(
            "ApgScanAssembly",
            syntaxTrees,
            references,
            new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, allowUnsafe: true));
    }

    /// <summary>
    /// Builds one project's compilation, named after the project, over its
    /// parsed trees with core BCL metadata references plus a compilation
    /// reference to each already-built referenced project's compilation.
    /// </summary>
    public static CSharpCompilation CreateProjectCompilation(
        CsharpProject project, List<SyntaxTree> syntaxTrees, IEnumerable<CSharpCompilation> referencedCompilations)
    {
        var references = GetDefaultMetadataReferences();
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

    internal static List<MetadataReference> GetDefaultMetadataReferences()
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
}
