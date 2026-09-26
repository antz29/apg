using System;
using System.Linq;
using Microsoft.CodeAnalysis;

namespace Apg.CsharpFrontend;

/// <summary>
/// Pure symbol→identity helpers: namespace/type/method FQN fragments, arity
/// suffixes, parameter-type display, emitted method names, and overload-group
/// keys. No scanner state — every method is a pure function of its symbol.
/// </summary>
internal static class SymbolNaming
{
    /// <summary>
    /// The `name` the frontend emits for a method-like symbol: a constructor
    /// carries its containing type's simple name, a property accessor
    /// `get_`/`set_` + the property's name (an indexer's property name is
    /// `this[]`), everything else its own (arity-suffixed) name.
    /// </summary>
    public static string EmittedMethodName(IMethodSymbol method)
    {
        if (method.MethodKind == MethodKind.Constructor)
        {
            return method.ContainingType?.Name ?? method.Name;
        }
        if (method.MethodKind is MethodKind.PropertyGet or MethodKind.PropertySet
            && method.AssociatedSymbol is IPropertySymbol prop)
        {
            string prefix = method.MethodKind == MethodKind.PropertyGet ? "get_" : "set_";
            return prefix + prop.Name;
        }
        return GetMethodNameWithArity(method);
    }

    public static string MethodGroupKey(IMethodSymbol method)
        => GetParentScopeFqn(method) + "\u0000" + EmittedMethodName(method);

    public static string GetParentScopeFqn(ISymbol symbol)
    {
        if (symbol.ContainingType != null)
        {
            return GetFullFqn(symbol.ContainingType);
        }
        return symbol.ContainingNamespace is { IsGlobalNamespace: false } ns ? ns.ToDisplayString() : "";
    }

    public static string GetTypeNameWithArity(INamedTypeSymbol symbol)
    {
        if (symbol.Arity > 0)
        {
            return $"{symbol.Name}`{symbol.Arity}";
        }
        return symbol.Name;
    }

    public static string GetMethodNameWithArity(IMethodSymbol symbol)
    {
        if (symbol.Arity > 0)
        {
            return $"{symbol.Name}`{symbol.Arity}";
        }
        return symbol.Name;
    }

    public static string GetFullFqn(ISymbol symbol)
    {
        if (symbol is INamespaceSymbol ns)
        {
            return ns.IsGlobalNamespace ? "" : ns.ToDisplayString();
        }
        if (symbol is INamedTypeSymbol namedType)
        {
            string typeName = GetTypeNameWithArity(namedType);
            if (symbol.ContainingType != null)
            {
                var parent = GetFullFqn(symbol.ContainingType);
                return string.IsNullOrEmpty(parent) ? typeName : $"{parent}.{typeName}";
            }
            if (symbol.ContainingNamespace is { IsGlobalNamespace: false } ns1)
            {
                return $"{ns1.ToDisplayString()}.{typeName}";
            }
            return typeName;
        }
        if (symbol.ContainingType != null)
        {
            var parent = GetFullFqn(symbol.ContainingType);
            return string.IsNullOrEmpty(parent) ? symbol.Name : $"{parent}.{symbol.Name}";
        }
        if (symbol.ContainingNamespace is { IsGlobalNamespace: false } ns2)
        {
            return $"{ns2.ToDisplayString()}.{symbol.Name}";
        }
        return symbol.Name;
    }

    public static string GetParameterTypeString(IParameterSymbol param)
    {
        var prefix = param.RefKind switch
        {
            RefKind.Out => "out ",
            RefKind.Ref => "ref ",
            RefKind.In or RefKind.RefReadOnlyParameter => "in ",
            _ => ""
        };
        return prefix + param.Type.ToDisplayString(SymbolDisplayFormat.MinimallyQualifiedFormat);
    }
}
