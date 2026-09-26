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
    /// <summary>Parses every discovered file into a syntax tree (UTF-8, latest language version).</summary>
    public static List<SyntaxTree> ParseTrees(IEnumerable<string> files)
    {
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
        return syntaxTrees;
    }

    /// <summary>
    /// Builds the compilation over the full syntax-tree set with core BCL
    /// metadata references. The compilation always covers the FULL file set: a
    /// target set filters emission, not resolution.
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
}
