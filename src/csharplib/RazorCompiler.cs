using Microsoft.AspNetCore.Mvc.Razor.Extensions;
using Microsoft.AspNetCore.Razor.Language;
using Microsoft.AspNetCore.Razor.Language.Intermediate;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Razor;

namespace Apg.CsharpFrontend;

/// <summary>
/// Compiles <c>.cshtml</c>/<c>.razor</c> files to generated C# through the
/// Razor 6.0 project engine.
/// </summary>
internal static class RazorCompiler
{
    /// <summary>
    /// The Razor 6.0 project engine for one project compilation (or the
    /// projectless one), rooted at <paramref name="projectDirectory"/> (the
    /// scan root for the projectless compilation): the ASP.NET Core MVC
    /// configuration (RazorExtensions) for every <c>.cshtml</c> and the
    /// component configuration for every <c>.razor</c>. Tag helpers come from
    /// a <see cref="SuppliedCompilationTagHelperFeature"/> in place of the
    /// stock CompilationTagHelperFeature; its compilation is supplied later,
    /// per compilation, never here.
    /// </summary>
    public static RazorProjectEngine CreateProjectEngine(string projectDirectory)
    {
        var configuration = RazorConfiguration.Create(RazorLanguageVersion.Version_6_0, "MVC-3.0", Array.Empty<RazorExtension>());
        return RazorProjectEngine.Create(configuration, RazorProjectFileSystem.Create(projectDirectory), builder =>
        {
            builder.Features.Add(new SuppliedCompilationTagHelperFeature());
            CompilerFeatures.Register(builder);
            RazorExtensions.Register(builder);
        });
    }

    /// <summary>
    /// An <see cref="ITagHelperFeature"/> whose descriptors are computed by the
    /// engine's registered <see cref="ITagHelperDescriptorProvider"/>s over
    /// the supplied <see cref="Compilation"/> — unlike the stock feature, it
    /// sees in-memory declaration trees. No compilation, no descriptors.
    /// </summary>
    internal sealed class SuppliedCompilationTagHelperFeature : RazorEngineFeatureBase, ITagHelperFeature
    {
        public Compilation? Compilation { get; set; }

        public IReadOnlyList<TagHelperDescriptor> GetDescriptors()
        {
            var results = new List<TagHelperDescriptor>();
            if (Compilation is null) return results;
            var context = TagHelperDescriptorProviderContext.Create(results);
            context.SetCompilation(Compilation);
            foreach (var provider in Engine.Features.OfType<ITagHelperDescriptorProvider>().OrderBy(p => p.Order))
                provider.Execute(context);
            return results;
        }
    }

    /// <summary>
    /// The import documents for one Razor file: every <c>_ViewImports.cshtml</c>
    /// (for a <c>.cshtml</c>) or <c>_Imports.razor</c> (for a <c>.razor</c>) in
    /// the file's directory and each ancestor up to and including
    /// <paramref name="rootDirectory"/> — the owning project's directory, or
    /// the scan root for a projectless file — outermost first, as the engine
    /// applies them.
    /// </summary>
    public static IReadOnlyList<RazorSourceDocument> CollectImports(string filePath, string rootDirectory)
    {
        var importName = filePath.EndsWith(".razor", StringComparison.OrdinalIgnoreCase) ? "_Imports.razor" : "_ViewImports.cshtml";
        var root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(rootDirectory));
        var imports = new List<RazorSourceDocument>();
        for (var dir = Path.GetDirectoryName(Path.GetFullPath(filePath)); dir is not null; dir = Path.GetDirectoryName(dir))
        {
            var import = Path.Combine(dir, importName);
            if (File.Exists(import))
                imports.Add(RazorSourceDocument.Create(File.ReadAllText(import), new RazorSourceDocumentProperties(import, Path.GetRelativePath(root, import))));
            if (string.Equals(Path.TrimEndingDirectorySeparator(dir), root, StringComparison.Ordinal)) break;
        }
        imports.Reverse();
        return imports;
    }

    /// <summary>
    /// The root namespace for one Razor file's generated code: the owning
    /// <paramref name="project"/>'s RootNamespace, else its project name, else
    /// (projectless, <paramref name="project"/> null) the global namespace,
    /// returned as the empty string. A <c>@namespace</c> directive overrides
    /// it — the file's own <paramref name="text"/> first, else the innermost
    /// of the applied <paramref name="imports"/> (outermost first, as
    /// <see cref="CollectImports"/> returns them); within a document the last
    /// directive wins.
    /// </summary>
    public static string ResolveRootNamespace(CsharpProject? project, string text, IReadOnlyList<RazorSourceDocument> imports)
    {
        var documents = imports.Select(i =>
        {
            var chars = new char[i.Length];
            i.CopyTo(0, chars, 0, i.Length);
            return new string(chars);
        }).Append(text);
        var directive = documents.Select(d => NamespaceDirectivePattern.Matches(d).LastOrDefault()?.Groups[1].Value).LastOrDefault(n => n is not null);
        if (directive is not null) return directive;
        if (project is null) return "";
        return string.IsNullOrWhiteSpace(project.RootNamespace) ? project.Name : project.RootNamespace.Trim();
    }

    /// <summary>
    /// The declaration-only Razor pass over every Razor file of one
    /// compilation: each of <paramref name="razorFiles"/> (absolute paths) is
    /// compiled with <see cref="CollectImports"/>' imports up to
    /// <paramref name="rootDirectory"/> and <see cref="ResolveRootNamespace"/>'s
    /// root namespace applied (the global namespace when it is empty), its
    /// primary method body suppressed, so the returned generated-C# trees
    /// declare the component and view types and can join the compilation
    /// before the full pass. Each tree's FilePath is its Razor file's own
    /// absolute path, at most one tree per file. Never throws on a Razor
    /// error: a file whose compile reports errors yields whatever tree the
    /// engine produced (possibly declaring nothing), a file the engine cannot
    /// process yields no tree, and every other file is still compiled. Razor
    /// diagnostics are not reported here — the full pass reports them.
    /// </summary>
    public static List<SyntaxTree> CompileDeclarations(RazorProjectEngine engine, CsharpProject? project, string rootDirectory, IEnumerable<string> razorFiles)
    {
        var trees = new List<SyntaxTree>();
        foreach (var filePath in razorFiles)
        {
            try
            {
                trees.Add(Generate(engine, project, rootDirectory, filePath, declarationsOnly: true).Tree);
            }
            catch (Exception)
            {
                // No tree for this file; the full pass reports its failure.
            }
        }
        return trees;
    }

    /// <summary>
    /// The full Razor pass over the Razor files of one compilation: first
    /// hands <paramref name="compilation"/> — the one already carrying
    /// <see cref="CompileDeclarations"/>' trees, its metadata references and
    /// its references to the referenced projects' Razor-carrying compilations —
    /// to the engine's <see cref="SuppliedCompilationTagHelperFeature"/>, so
    /// child component tags bind to components declared in this or a
    /// referenced project; then compiles each of <paramref name="razorFiles"/>
    /// (absolute paths) with the same imports and root namespace as
    /// <see cref="CompileDeclarations"/>. Returns, per file in order, its
    /// <c>#line</c>-carrying generated-C# tree, whose FilePath is the Razor
    /// file's own absolute path, and that file's Razor error diagnostics.
    /// Never throws on a Razor error: an erroring file still yields whatever
    /// tree the engine produced (possibly declaring nothing) with its errors.
    /// </summary>
    public static List<(SyntaxTree Tree, IReadOnlyList<RazorDiagnostic> Errors)> Compile(
        RazorProjectEngine engine, CsharpProject? project, string rootDirectory, IEnumerable<string> razorFiles, Compilation compilation)
    {
        engine.EngineFeatures.OfType<SuppliedCompilationTagHelperFeature>().Single().Compilation = compilation;
        return razorFiles.Select(filePath => Generate(engine, project, rootDirectory, filePath, declarationsOnly: false)).ToList();
    }

    /// <summary>
    /// Compiles one Razor file with its imports and resolved root namespace
    /// (shared by both passes so they declare the same types); with
    /// <paramref name="declarationsOnly"/> the primary method body is suppressed.
    /// </summary>
    private static (SyntaxTree Tree, IReadOnlyList<RazorDiagnostic> Errors) Generate(
        RazorProjectEngine engine, CsharpProject? project, string rootDirectory, string filePath, bool declarationsOnly)
    {
        const string GlobalSentinel = "__ApgGlobalNamespace"; // lets Razor compute class names and folder suffixes, stripped below
        var text = File.ReadAllText(filePath);
        var imports = CollectImports(filePath, rootDirectory);
        var rootNamespace = ResolveRootNamespace(project, text, imports);
        var fileKind = FileKinds.GetFileKindFromFilePath(filePath);
        var source = RazorSourceDocument.Create(text, new RazorSourceDocumentProperties(filePath, Path.GetRelativePath(rootDirectory, filePath)));
        var parserOptions = RazorParserOptions.Create(options =>
        {
            foreach (var feature in engine.EngineFeatures.OfType<IConfigureRazorParserOptionsFeature>().OrderBy(f => f.Order))
                feature.Configure(options);
        }, fileKind);
        var codeGenerationOptions = RazorCodeGenerationOptions.Create(options =>
        {
            foreach (var feature in engine.EngineFeatures.OfType<IConfigureRazorCodeGenerationOptionsFeature>().OrderBy(f => f.Order))
                feature.Configure(options);
            options.RootNamespace = rootNamespace.Length > 0 ? rootNamespace : GlobalSentinel;
            options.SuppressPrimaryMethodBody = declarationsOnly;
        });
        var document = RazorCodeDocument.Create(source, imports, parserOptions, codeGenerationOptions);
        document.SetFileKind(fileKind);
        foreach (var phase in engine.Engine.Phases)
        {
            // Razor's defaults (MVC's `AspNetCore`, a component's fallback) and the sentinel give way to the resolved root namespace.
            if (phase is IRazorOptimizationPhase && document.GetDocumentIntermediateNode().FindPrimaryNamespace() is { } ns)
            {
                if (ns.Content is "AspNetCore" or "__GeneratedComponent" or GlobalSentinel)
                    ns.Content = rootNamespace;
                else if (ns.Content.StartsWith(GlobalSentinel + ".", StringComparison.Ordinal))
                    ns.Content = ns.Content[(GlobalSentinel.Length + 1)..];
            }
            // The global namespace: its members move up to the document (optimization passes still need the node, codegen cannot write it).
            if (phase is IRazorCSharpLoweringPhase && document.GetDocumentIntermediateNode().FindPrimaryNamespace() is { Content.Length: 0 } global)
            {
                var children = document.GetDocumentIntermediateNode().Children;
                var at = children.IndexOf(global);
                children.RemoveAt(at);
                foreach (var child in global.Children.Reverse())
                    children.Insert(at, child);
            }
            phase.Execute(document);
        }
        var csharp = document.GetCSharpDocument();
        var errors = csharp.Diagnostics.Where(d => d.Severity == RazorDiagnosticSeverity.Error).ToList();
        return (CSharpSyntaxTree.ParseText(csharp.GeneratedCode, new CSharpParseOptions(LanguageVersion.Latest), filePath), errors);
    }

    /// <summary>A line-leading <c>@namespace</c> directive and its namespace.</summary>
    private static readonly System.Text.RegularExpressions.Regex NamespaceDirectivePattern =
        new(@"^[ \t]*@namespace[ \t]+([^\s;]+)", System.Text.RegularExpressions.RegexOptions.Multiline);

    /// <summary>
    /// An empty project file system, so the engine never reads the disk;
    /// sources are handed to it directly.
    /// </summary>
    private sealed class InMemoryFileSystem : RazorProjectFileSystem
    {
        public override IEnumerable<RazorProjectItem> EnumerateItems(string basePath) => Array.Empty<RazorProjectItem>();
        public override RazorProjectItem GetItem(string path) => new NotFoundItem(path, null);
        public override RazorProjectItem GetItem(string path, string? fileKind) => new NotFoundItem(path, fileKind);
    }

    /// <summary>
    /// A missing project item — stands in for Razor 6.0's internal
    /// NotFoundProjectItem, which is not reachable from outside the assembly.
    /// </summary>
    private sealed class NotFoundItem(string path, string? fileKind) : RazorProjectItem
    {
        public override string BasePath => string.Empty;
        public override string FilePath => path;
        public override string? PhysicalPath => null;
        public override string? RelativePhysicalPath => null;
        public override string FileKind => fileKind ?? FileKinds.GetFileKindFromFilePath(path);
        public override bool Exists => false;
        public override Stream Read() => throw new InvalidOperationException($"Razor project item not found: {path}");
    }
}
