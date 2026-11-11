using System;
using Microsoft.CodeAnalysis;

namespace Apg.CsharpFrontend;

/// <summary>
/// Unresolved-target category classification: Roslyn stdlib
/// (<c>System.*</c>/<c>Microsoft.*</c>) vs external, with <c>unknown</c> for a
/// missing or unbound (error) type.
/// </summary>
internal static class UnresolvedClassifier
{
    public static string ClassifyCategory(ITypeSymbol? type)
    {
        // Unbound (metadata absent locally): origin can't be known, so never guess stdlib/external.
        if (type == null || type is IErrorTypeSymbol || type.TypeKind == TypeKind.Error) return "unknown";
        var ns = type.ContainingNamespace?.ToDisplayString() ?? "";
        if (ns.StartsWith("System", StringComparison.Ordinal) || ns.StartsWith("Microsoft", StringComparison.Ordinal))
        {
            return "stdlib";
        }
        return "external";
    }
}
