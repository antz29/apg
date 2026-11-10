using System;
using System.Collections.Generic;
using System.Linq;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace Apg.CsharpFrontend;

/// <summary>
/// Pass 2 — semantic edge emission: `calls` / `uses` / `unresolved_call` /
/// `unresolved_use` edges plus the unresolved-target nodes they reference.
/// Holds no state of its own: the symbol→id map, unresolved dedup, and endpoint
/// resolution live on the shared <see cref="Scanner"/>.
/// </summary>
internal sealed class EdgeEmitter
{
    private readonly Scanner _scanner;

    public EdgeEmitter(Scanner scanner)
    {
        _scanner = scanner;
    }

    public void EmitEdges(SyntaxTree tree, SemanticModel model, CompilationUnitSyntax root)
    {
        foreach (var node in root.DescendantNodes())
        {
            // Resolve Calls from Invocation Expressions
            if (node is InvocationExpressionSyntax invocation)
            {
                var callerSymbol = GetEnclosingExecutableSymbol(node, model);
                if (callerSymbol != null && _scanner.SymbolToId.TryGetValue(callerSymbol, out var callerId))
                {
                    var symbolInfo = model.GetSymbolInfo(invocation);
                    var targetSymbol = symbolInfo.Symbol ?? symbolInfo.CandidateSymbols.FirstOrDefault();

                    if (targetSymbol is IMethodSymbol targetMethod)
                    {
                        if (_scanner.TryResolveEndpoint(targetMethod, out var targetEndpoint))
                        {
                            JsonlWriter.Write(new EdgeMsg
                            {
                                Type = "calls",
                                From = callerId,
                                To = targetEndpoint
                            });
                        }
                        else
                        {
                            EmitUnresolvedCall(callerId, targetMethod);
                        }
                    }
                    else if (targetSymbol is IFieldSymbol or ILocalSymbol or IParameterSymbol)
                    {
                        // Delegate call / Func-value call
                        JsonlWriter.Write(new EdgeMsg
                        {
                            Type = "unresolved_call",
                            From = callerId,
                            To = invocation.Expression.ToString(),
                            TargetType = targetSymbol.ToString()
                        });
                    }
                }
            }
            // Resolve Calls from Object Creations (constructors)
            else if (node is ObjectCreationExpressionSyntax creation)
            {
                var callerSymbol = GetEnclosingExecutableSymbol(node, model);
                if (callerSymbol != null && _scanner.SymbolToId.TryGetValue(callerSymbol, out var callerId))
                {
                    var symbolInfo = model.GetSymbolInfo(creation);
                    var targetSymbol = symbolInfo.Symbol ?? symbolInfo.CandidateSymbols.FirstOrDefault();

                    if (targetSymbol is IMethodSymbol targetCtor)
                    {
                        if (_scanner.TryResolveEndpoint(targetCtor, out var targetEndpoint))
                        {
                            JsonlWriter.Write(new EdgeMsg
                            {
                                Type = "calls",
                                From = callerId,
                                To = targetEndpoint
                            });
                        }
                        else
                        {
                            EmitUnresolvedCall(callerId, targetCtor);
                        }
                    }
                    else
                    {
                        // Fall back to Uses edge to created type
                        var typeInfo = model.GetTypeInfo(creation);
                        if (typeInfo.Type is INamedTypeSymbol createdType)
                        {
                            EmitTypeUse(callerId, createdType);
                        }
                    }
                }
            }
            // Resolve Uses from Type Declarations (Base Types / Interfaces)
            else if (node is BaseTypeDeclarationSyntax baseTypeDecl)
            {
                var declaredType = model.GetDeclaredSymbol(baseTypeDecl);
                if (declaredType != null && _scanner.SymbolToId.TryGetValue(declaredType, out var structId))
                {
                    if (declaredType.BaseType != null && declaredType.BaseType.SpecialType != SpecialType.System_Object)
                    {
                        EmitTypeUse(structId, declaredType.BaseType);
                    }
                    foreach (var iface in declaredType.Interfaces)
                    {
                        EmitTypeUse(structId, iface);
                    }
                }
            }
            // Resolve Uses from Type Syntax (Fields, Properties, Local variables, Casts)
            else if (node is TypeSyntax typeSyntax && node.Parent is not (BaseTypeDeclarationSyntax or NamespaceDeclarationSyntax or FileScopedNamespaceDeclarationSyntax))
            {
                var enclosingSymbol = GetEnclosingExecutableSymbol(node, model) ?? GetEnclosingTypeSymbol(node, model);
                if (enclosingSymbol != null && _scanner.SymbolToId.TryGetValue(enclosingSymbol, out var fromId))
                {
                    var typeInfo = model.GetTypeInfo(typeSyntax);
                    if (typeInfo.Type is INamedTypeSymbol usedType)
                    {
                        EmitTypeUse(fromId, usedType);
                    }
                }
            }
        }
    }

    private void EmitTypeUse(string fromId, INamedTypeSymbol targetType)
    {
        if (targetType.SpecialType != SpecialType.None && targetType.SpecialType != SpecialType.System_Object)
            return;

        if (_scanner.TryResolveEndpoint(targetType, out var targetEndpoint))
        {
            JsonlWriter.Write(new EdgeMsg
            {
                Type = "uses",
                From = fromId,
                To = targetEndpoint
            });
        }
        else
        {
            string fqn = SymbolNaming.GetFullFqn(targetType);
            if (string.IsNullOrEmpty(fqn)) return;

            EmitUnresolvedNode(fqn, UnresolvedClassifier.ClassifyCategory(targetType));
            JsonlWriter.Write(new EdgeMsg
            {
                Type = "unresolved_use",
                From = fromId,
                To = fqn
            });
        }
    }

    private void EmitUnresolvedCall(string fromId, IMethodSymbol targetMethod)
    {
        string parentFqn = SymbolNaming.GetParentScopeFqn(targetMethod);
        string methodName = SymbolNaming.GetMethodNameWithArity(targetMethod);
        string fqn = string.IsNullOrEmpty(parentFqn) ? methodName : $"{parentFqn}.{methodName}";
        string category = UnresolvedClassifier.ClassifyCategory(targetMethod.ContainingType);

        EmitUnresolvedNode(fqn, category);
        JsonlWriter.Write(new EdgeMsg
        {
            Type = "unresolved_call",
            From = fromId,
            To = fqn
        });
    }

    private void EmitUnresolvedNode(string fqn, string category)
    {
        if (_scanner.EmittedUnresolved.Add(fqn))
        {
            JsonlWriter.Write(new UnresolvedMsg
            {
                Fqn = fqn,
                Category = category
            });
        }
    }

    private static ISymbol? GetEnclosingExecutableSymbol(SyntaxNode node, SemanticModel model)
    {
        var current = node.Parent;
        while (current != null)
        {
            if (current is MethodDeclarationSyntax or ConstructorDeclarationSyntax or AccessorDeclarationSyntax or LocalFunctionStatementSyntax)
            {
                return model.GetDeclaredSymbol(current);
            }
            current = current.Parent;
        }
        return null;
    }

    private static ISymbol? GetEnclosingTypeSymbol(SyntaxNode node, SemanticModel model)
    {
        var current = node.Parent;
        while (current != null)
        {
            if (current is BaseTypeDeclarationSyntax typeDecl)
            {
                return model.GetDeclaredSymbol(typeDecl);
            }
            current = current.Parent;
        }
        return null;
    }
}
