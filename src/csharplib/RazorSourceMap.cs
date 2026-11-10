using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Text;

namespace Apg.CsharpFrontend;

/// <summary>
/// Maps a span in Razor-generated C# back to its <c>.cshtml</c>/<c>.razor</c>
/// source through the generated tree's <c>#line</c> directives.
/// </summary>
internal static class RazorSourceMap
{
    /// <summary>
    /// The 1-based inclusive Razor source line span (and source path) of a
    /// generated-C# span, or null when no <c>#line</c> directive maps it.
    /// </summary>
    public static (string Path, int StartLine, int EndLine)? MapSpan(SyntaxTree generated, TextSpan span)
    {
        var mapped = generated.GetMappedLineSpan(span);
        if (!mapped.IsValid || !mapped.HasMappedPath) return null;
        return (mapped.Path, mapped.StartLinePosition.Line + 1, mapped.EndLinePosition.Line + 1);
    }
}
