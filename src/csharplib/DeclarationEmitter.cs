using System;
using System.Collections.Generic;
using System.Linq;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace Apg.CsharpFrontend;

/// <summary>
/// Pass 1 — declaration emission: per-file file/module scaffolding, struct and
/// function (method/constructor/accessor) nodes, the module hierarchy, and the
/// `contains` edges. Holds no state of its own: id allocation, the symbol/decl
/// maps, and module registration live on the shared <see cref="Scanner"/>.
/// </summary>
internal sealed class DeclarationEmitter
{
    private readonly Scanner _scanner;

    public DeclarationEmitter(Scanner scanner)
    {
        _scanner = scanner;
    }

    /// <summary>
    /// Emits one whole-path module per readable <c>.sln</c> and per loaded
    /// project (keyed by absolute file path; the ingestor renders it
    /// repo-relative as one segment), plus a <c>contains</c> edge from each
    /// solution to every loaded project it lists. A project several solutions
    /// list is one module under each; an unlisted project stays top-level.
    /// </summary>
    public void EmitProjectModules(IEnumerable<string> solutionPaths, IEnumerable<CsharpProject> projects)
    {
        var loaded = projects.Select(p => p.Path).ToHashSet(StringComparer.Ordinal);
        var listed = new List<(string Sln, List<string> Projects)>();
        foreach (var sln in solutionPaths.Select(Path.GetFullPath).Distinct(StringComparer.Ordinal).OrderBy(s => s, StringComparer.Ordinal))
        {
            List<string> members;
            try
            {
                members = SolutionReader.Read(sln);
            }
            catch
            {
                continue; // unreadable .sln: already warned and skipped by project discovery
            }
            listed.Add((sln, members.Where(loaded.Contains).OrderBy(p => p, StringComparer.Ordinal).ToList()));
        }

        foreach (var fqn in listed.Select(l => l.Sln).Concat(loaded.OrderBy(p => p, StringComparer.Ordinal)))
        {
            if (_scanner.EmittedModules.Add(fqn)) JsonlWriter.Write(new ModuleMsg { Fqn = fqn });
        }
        foreach (var (sln, members) in listed)
        {
            foreach (var p in members) JsonlWriter.Write(new EdgeMsg { Type = "contains", From = sln, To = p });
        }
    }

    /// <summary>
    /// Emits one file and its declarations from the compilation it is passed in
    /// — the caller passes a shared file once, with its owning project's model.
    /// A project file (non-null <paramref name="owner"/>) is parented to its
    /// project module; a projectless .cs file keeps its namespace parent and a
    /// .csx script its own script module. A Razor-generated tree (FilePath a
    /// .cshtml/.razor file) gets its File record under that Razor path, spanning
    /// the Razor file's own line count, with the same parent rules. Collision members' struct records
    /// carry their qualified parent (<see cref="SymbolNaming.GetParentScopeFqn"/>).
    /// </summary>
    public void EmitFileAndDeclarations(SyntaxTree tree, SemanticModel model, CompilationUnitSyntax root, bool emit, CsharpProject? owner = null)
    {
        var filePath = tree.FilePath;
        bool isRazor = filePath.EndsWith(".cshtml", StringComparison.OrdinalIgnoreCase)
            || filePath.EndsWith(".razor", StringComparison.OrdinalIgnoreCase);
        // A Razor tree's FilePath is its .cshtml/.razor file: the File record spans that file's own lines, never the generated C#'s.
        var text = isRazor ? Microsoft.CodeAnalysis.Text.SourceText.From(File.ReadAllText(filePath, System.Text.Encoding.UTF8)) : tree.GetText();
        int totalLines = Math.Max(1, text.Lines.Count);

        // Determine parent module / namespace for file
        string defaultNamespace = "";
        var firstNs = root.DescendantNodes().OfType<BaseNamespaceDeclarationSyntax>().FirstOrDefault();
        if (firstNs != null)
        {
            defaultNamespace = firstNs.Name.ToString();
        }

        // Module scaffolding is global (it is not part of a per-file fact unit),
        // so it is emitted for every file — target or not — and the cached splice
        // sees the full Module→Module hierarchy.
        bool isScript = tree.Options.Kind == SourceCodeKind.Script;
        if (isScript)
        {
            // A script is its own whole-path module (no dot-split, no parent edge); the ingestor renders it repo-relative.
            defaultNamespace = filePath;
            if (_scanner.EmittedModules.Add(filePath))
            {
                JsonlWriter.Write(new ModuleMsg { Fqn = filePath });
            }
        }
        else if (!string.IsNullOrEmpty(defaultNamespace))
        {
            EmitModuleHierarchy(defaultNamespace);
        }

        // Emit File Node
        if (emit)
        {
            JsonlWriter.Write(new FileMsg
            {
                Path = filePath,
                Parent = !isScript && owner != null ? owner.Path : defaultNamespace,
                StartLine = 1,
                EndLine = totalLines
            });
        }

        // Traverse declarations in file
        foreach (var member in root.DescendantNodes())
        {
            if (member is BaseTypeDeclarationSyntax typeDecl)
            {
                var symbol = model.GetDeclaredSymbol(typeDecl);
                if (symbol == null) continue;

                string parentFqn = SymbolNaming.GetParentScopeFqn(symbol);
                // A nested type's parent is its containing type, never a module; its modules came with its outer type.
                if (symbol.ContainingType == null && !isScript && !string.IsNullOrEmpty(parentFqn))
                {
                    EmitModuleHierarchy(parentFqn);
                }

                // Handle partial classes / types: if symbol already has an assigned ID, reuse it
                // and avoid emitting duplicate Struct node (which causes FQN collision).
                if (!_scanner.SymbolToId.TryGetValue(symbol, out var id))
                {
                    // A non-target file registers no id: its declarations are
                    // served from the shared fact cache, and its cross-file
                    // endpoints are emitted by canonical FQN.
                    if (!emit) continue;

                    id = _scanner.NextId();
                    _scanner.SymbolToId[symbol] = id;

                    var span = typeDecl.Span;
                    var lineSpan = tree.GetLineSpan(span);

                    string name = SymbolNaming.GetTypeNameWithArity(symbol);
                    string fullFqn = string.IsNullOrEmpty(parentFqn) ? name : $"{parentFqn}.{name}";
                    _scanner.DeclFqnToId[fullFqn] = id;

                    JsonlWriter.Write(new StructMsg
                    {
                        Id = id,
                        Parent = parentFqn,
                        Name = name,
                        Path = filePath,
                        Start = span.Start,
                        End = span.End,
                        StartLine = lineSpan.StartLinePosition.Line + 1,
                        EndLine = lineSpan.EndLinePosition.Line + 1
                    });

                    // If nested type, emit Struct -> Struct Contains edge
                    if (symbol.ContainingType != null && _scanner.TryResolveEndpoint(symbol.ContainingType, out var parentStructEndpoint))
                    {
                        JsonlWriter.Write(new EdgeMsg
                        {
                            Type = "contains",
                            From = parentStructEndpoint,
                            To = id
                        });
                    }
                }
            }
            else if (emit && member is MethodDeclarationSyntax methodDecl)
            {
                EmitMethod(methodDecl, model, tree, filePath);
            }
            else if (emit && member is ConstructorDeclarationSyntax ctorDecl)
            {
                EmitConstructor(ctorDecl, model, tree, filePath);
            }
            else if (emit && member is PropertyDeclarationSyntax propDecl)
            {
                EmitPropertyAccessors(propDecl, model, tree, filePath);
            }
        }
    }

    private void EmitMethod(MethodDeclarationSyntax methodDecl, SemanticModel model, SyntaxTree tree, string filePath)
    {
        var symbol = model.GetDeclaredSymbol(methodDecl);
        if (symbol == null) return;

        var id = _scanner.NextId();
        _scanner.SymbolToId[symbol] = id;

        var span = methodDecl.Span;
        var lineSpan = tree.GetLineSpan(span);
        string parentFqn = SymbolNaming.GetParentScopeFqn(symbol);

        var paramTypes = symbol.Parameters.Select(SymbolNaming.GetParameterTypeString).ToList();

        string name = SymbolNaming.GetMethodNameWithArity(symbol);
        string funcKey = $"{parentFqn}.{name}";
        _scanner.DeclFqnToId[funcKey] = id;

        JsonlWriter.Write(new FuncMsg
        {
            Id = id,
            Parent = parentFqn,
            Name = name,
            Params = paramTypes,
            File = filePath,
            Path = filePath,
            Start = span.Start,
            End = span.End,
            StartLine = lineSpan.StartLinePosition.Line + 1,
            EndLine = lineSpan.EndLinePosition.Line + 1
        });

        // If enclosed in a type, emit Struct -> Function Contains edge
        if (symbol.ContainingType != null && _scanner.TryResolveEndpoint(symbol.ContainingType, out var parentStructEndpoint))
        {
            JsonlWriter.Write(new EdgeMsg
            {
                Type = "contains",
                From = parentStructEndpoint,
                To = id
            });
        }
    }

    private void EmitConstructor(ConstructorDeclarationSyntax ctorDecl, SemanticModel model, SyntaxTree tree, string filePath)
    {
        var symbol = model.GetDeclaredSymbol(ctorDecl);
        if (symbol == null) return;

        var id = _scanner.NextId();
        _scanner.SymbolToId[symbol] = id;

        var span = ctorDecl.Span;
        var lineSpan = tree.GetLineSpan(span);
        string parentFqn = SymbolNaming.GetParentScopeFqn(symbol);

        var paramTypes = symbol.Parameters.Select(SymbolNaming.GetParameterTypeString).ToList();
        string name = symbol.ContainingType?.Name ?? symbol.Name;
        string funcKey = $"{parentFqn}.{name}";
        _scanner.DeclFqnToId[funcKey] = id;

        JsonlWriter.Write(new FuncMsg
        {
            Id = id,
            Parent = parentFqn,
            Name = name,
            Params = paramTypes,
            File = filePath,
            Path = filePath,
            Start = span.Start,
            End = span.End,
            StartLine = lineSpan.StartLinePosition.Line + 1,
            EndLine = lineSpan.EndLinePosition.Line + 1
        });

        if (symbol.ContainingType != null && _scanner.TryResolveEndpoint(symbol.ContainingType, out var parentStructEndpoint))
        {
            JsonlWriter.Write(new EdgeMsg
            {
                Type = "contains",
                From = parentStructEndpoint,
                To = id
            });
        }
    }

    private void EmitPropertyAccessors(PropertyDeclarationSyntax propDecl, SemanticModel model, SyntaxTree tree, string filePath)
    {
        var propSymbol = model.GetDeclaredSymbol(propDecl);
        if (propSymbol == null) return;

        if (propSymbol.GetMethod != null)
        {
            EmitAccessor(propSymbol.GetMethod, propDecl, tree, filePath, $"get_{propSymbol.Name}");
        }
        if (propSymbol.SetMethod != null)
        {
            EmitAccessor(propSymbol.SetMethod, propDecl, tree, filePath, $"set_{propSymbol.Name}");
        }
    }

    private void EmitAccessor(IMethodSymbol symbol, PropertyDeclarationSyntax propDecl, SyntaxTree tree, string filePath, string name)
    {
        var id = _scanner.NextId();
        _scanner.SymbolToId[symbol] = id;

        var span = propDecl.Span;
        var lineSpan = tree.GetLineSpan(span);
        string parentFqn = SymbolNaming.GetParentScopeFqn(symbol);

        var paramTypes = symbol.Parameters.Select(SymbolNaming.GetParameterTypeString).ToList();
        string funcKey = $"{parentFqn}.{name}";
        _scanner.DeclFqnToId[funcKey] = id;

        JsonlWriter.Write(new FuncMsg
        {
            Id = id,
            Parent = parentFqn,
            Name = name,
            Params = paramTypes,
            File = filePath,
            Path = filePath,
            Start = span.Start,
            End = span.End,
            StartLine = lineSpan.StartLinePosition.Line + 1,
            EndLine = lineSpan.EndLinePosition.Line + 1
        });

        if (symbol.ContainingType != null && _scanner.TryResolveEndpoint(symbol.ContainingType, out var parentStructEndpoint))
        {
            JsonlWriter.Write(new EdgeMsg
            {
                Type = "contains",
                From = parentStructEndpoint,
                To = id
            });
        }
    }

    private void EmitModuleHierarchy(string fqn)
    {
        if (string.IsNullOrEmpty(fqn)) return;
        string current = "";
        string rest = fqn;
        if (Path.IsPathRooted(fqn))
        {
            // Qualified collision parent: start under the already-emitted project module (EmitProjectModules), its path never split at dots.
            for (int dot = fqn.IndexOf('.'); ; dot = fqn.IndexOf('.', dot + 1))
            {
                var prefix = dot < 0 ? fqn : fqn[..dot];
                if (_scanner.EmittedModules.Contains(prefix))
                {
                    current = prefix;
                    rest = dot < 0 ? "" : fqn[(dot + 1)..];
                    break;
                }
                if (dot < 0) break;
            }
            if (rest.Length == 0) return; // global-namespace member: its parent IS the project module
        }
        var parts = rest.Split('.');

        for (int i = 0; i < parts.Length; i++)
        {
            string prev = current;
            current = current.Length == 0 ? parts[0] : $"{current}.{parts[i]}";

            if (_scanner.EmittedModules.Add(current))
            {
                JsonlWriter.Write(new ModuleMsg { Fqn = current });
                if (!string.IsNullOrEmpty(prev))
                {
                    JsonlWriter.Write(new EdgeMsg
                    {
                        Type = "contains",
                        From = prev,
                        To = current
                    });
                }
            }
        }
    }
}
