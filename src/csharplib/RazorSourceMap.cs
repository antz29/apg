using System;
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
    /// The 1-based inclusive line span, in the tree's own Razor file
    /// (<see cref="SyntaxTree.FilePath"/>), of a generated-C# span through its
    /// <c>#line</c> mapping; a span outside every <c>#line</c> mapping (the
    /// generated class, its render method) gets the whole-file span
    /// <c>1..razorLineCount</c> with <c>Mapped</c> false.
    /// </summary>
    public static (int StartLine, int EndLine, bool Mapped) MapSpan(SyntaxTree generated, TextSpan span, int razorLineCount)
    {
        var mapped = generated.GetMappedLineSpan(span);
        // HasMappedPath alone is not enough: the tree's FilePath is the Razor path and
        // `#line hidden` can be lost (global-namespace unwrap), so require visible, self-mapped ends.
        if (mapped.IsValid && mapped.HasMappedPath && mapped.Path == generated.FilePath
            && Visible(generated, span.Start) && Visible(generated, Math.Max(span.Start, span.End - 1)))
            return (mapped.StartLinePosition.Line + 1, mapped.EndLinePosition.Line + 1, true);
        return (1, Math.Max(1, razorLineCount), false);
    }

    private static bool Visible(SyntaxTree generated, int position) =>
        generated.GetLineVisibility(position) == LineVisibility.Visible;
}
