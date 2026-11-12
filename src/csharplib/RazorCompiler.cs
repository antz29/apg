using Microsoft.AspNetCore.Mvc.Razor.Extensions;
using Microsoft.AspNetCore.Razor.Language;
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
    /// Compiles one <c>.cshtml</c> (MVC view) or <c>.razor</c> (component)
    /// file's in-memory <paramref name="text"/> to a generated-C# syntax tree
    /// whose <c>#line</c> directives name <paramref name="filePath"/>, so
    /// <see cref="RazorSourceMap.MapSpan"/> maps its declarations back to it.
    /// Throws <see cref="InvalidOperationException"/> on any Razor error.
    /// </summary>
    public static SyntaxTree Compile(RazorProjectEngine engine, string filePath, string text)
    {
        var source = RazorSourceDocument.Create(text, new RazorSourceDocumentProperties(filePath, Path.GetFileName(filePath)));
        var csharp = engine.Process(source, FileKinds.GetFileKindFromFilePath(filePath), Array.Empty<RazorSourceDocument>(), Array.Empty<TagHelperDescriptor>())
            .GetCSharpDocument();
        var errors = csharp.Diagnostics.Where(d => d.Severity == RazorDiagnosticSeverity.Error).ToList();
        if (errors.Count > 0)
            throw new InvalidOperationException($"Razor compile failed for {filePath}: {string.Join("; ", errors)}");
        return CSharpSyntaxTree.ParseText(csharp.GeneratedCode, new CSharpParseOptions(LanguageVersion.Latest), filePath + ".g.cs");
    }

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
