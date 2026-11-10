using Microsoft.AspNetCore.Mvc.Razor.Extensions;
using Microsoft.AspNetCore.Razor.Language;

namespace Apg.CsharpFrontend;

/// <summary>
/// Compiles <c>.cshtml</c>/<c>.razor</c> files to generated C# through the
/// Razor 6.0 project engine.
/// </summary>
internal static class RazorCompiler
{
    /// <summary>
    /// A Razor 6.0 project engine with the MVC view (RazorExtensions) and the
    /// built-in component features. No tag-helper discovery and no metadata
    /// references, so nothing reads a loaded assembly's file path — it runs
    /// inside the single-file publish.
    /// </summary>
    public static RazorProjectEngine CreateProjectEngine(string root)
    {
        var configuration = RazorConfiguration.Create(RazorLanguageVersion.Version_6_0, "MVC-3.0", Array.Empty<RazorExtension>());
        return RazorProjectEngine.Create(configuration, RazorProjectFileSystem.Create(root), RazorExtensions.Register);
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
