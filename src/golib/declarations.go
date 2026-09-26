package main

import (
	"go/ast"
	"go/token"

	"golang.org/x/tools/go/packages"
)

// fileDecl is one declared struct/function node assigned an opaque id.
type fileDecl struct {
	kind    string // "struct" | "function"
	id      string
	parent  string
	name    string
	params  []string
	file    string
	path    string
	start   int
	end     int
	astNode ast.Node // *ast.TypeSpec (struct) or *ast.FuncDecl (function)
}

// collectDecls walks one file's top-level declarations, assigning ids and
// populating the structID/funcID maps.
func collectDecls(file *ast.File, filePath string, p *packages.Package) []fileDecl {
	pkgFqn := p.PkgPath
	ti := p.TypesInfo
	var decls []fileDecl

	for _, decl := range file.Decls {
		switch d := decl.(type) {
		case *ast.GenDecl:
			for _, spec := range d.Specs {
				ts, ok := spec.(*ast.TypeSpec)
				if !ok {
					continue
				}
				typeName := ts.Name.Name
				if typeName == "_" || typeName == "" {
					continue
				}
				id := newNodeID()
				structID[pkgFqn+"."+typeName] = id
				decls = append(decls, fileDecl{
					kind:    "struct",
					id:      id,
					parent:  pkgFqn,
					name:    typeName,
					path:    filePath,
					start:   spanStart(d, p),
					end:     p.Fset.Position(ts.End()).Offset,
					astNode: ts,
				})
				if iface, ok := ts.Type.(*ast.InterfaceType); ok {
					for _, m := range iface.Methods.List {
						if len(m.Names) == 0 {
							continue
						}
						for _, name := range m.Names {
							mid := newNodeID()
							parent := pkgFqn + "." + typeName
							funcID[funcKey(parent, name.Name, filePath)] = mid
							decls = append(decls, fileDecl{
								kind:    "function",
								id:      mid,
								parent:  parent,
								name:    name.Name,
								params:  interfaceMethodParams(m, ti),
								file:    filePath,
								path:    filePath,
								start:   p.Fset.Position(m.Pos()).Offset,
								end:     p.Fset.Position(m.End()).Offset,
								astNode: m,
							})
						}
					}
				}
			}

		case *ast.FuncDecl:
			funcName := d.Name.Name
			if funcName == "_" || funcName == "" {
				continue
			}
			var parentFqn string
			if d.Recv != nil && len(d.Recv.List) > 0 {
				recvType := recvTypeNameAST(d.Recv.List[0].Type)
				if recvType == "" {
					continue
				}
				parentFqn = pkgFqn + "." + recvType
			} else {
				parentFqn = pkgFqn
			}
			id := newNodeID()
			funcID[funcKey(parentFqn, funcName, filePath)] = id
			decls = append(decls, fileDecl{
				kind:    "function",
				id:      id,
				parent:  parentFqn,
				name:    funcName,
				params:  funcParams(d, p),
				file:    filePath,
				path:    filePath,
				start:   spanStart(d, p),
				end:     p.Fset.Position(d.End()).Offset,
				astNode: d,
			})
		}
	}
	return decls
}

// declTokenFile returns the token.File for a declaration's AST node, or nil if
// the decl has no AST node (defensive: line numbers degrade to 1 then).
func declTokenFile(p *packages.Package, d fileDecl) *token.File {
	if d.astNode == nil {
		return nil
	}
	return p.Fset.File(d.astNode.Pos())
}

// emitFile emits node + contains records for the file's declarations and walks
// struct bodies / function bodies for use, call, and unresolved edges.
func emitFile(file *ast.File, filePath string, p *packages.Package, decls []fileDecl, modSet map[string]bool) {
	ti := p.TypesInfo

	for _, d := range decls {
		switch d.kind {
		case "struct":
			f := declTokenFile(p, d)
			enc.Encode(structMsg{
				Type: "struct", ID: d.id, Parent: d.parent, Name: d.name,
				Path: d.path, Start: d.start, End: d.end,
				StartLine: offsetLine(f, d.start),
				EndLine:   offsetLine(f, d.end-1),
			})
			// The file (emitted as a file node) contains the struct; the module
			// reaches it through the file (SPEC §7).
			emitStructUses(d, ti, modSet)
		case "function":
			f := declTokenFile(p, d)
			enc.Encode(funcMsg{
				Type: "function", ID: d.id, Parent: d.parent, Name: d.name,
				Params: d.params, File: d.file, Path: d.path, Start: d.start, End: d.end,
				StartLine: offsetLine(f, d.start),
				EndLine:   offsetLine(f, d.end-1),
			})
			// Methods stay directly under their struct; free functions are
			// reached through the file node instead of the module.
			if id, ok := structID[d.parent]; ok {
				enc.Encode(edgeMsg{Type: "contains", From: edgeEndpoint(d.parent, id), To: d.id})
			}
			if fn, ok := d.astNode.(*ast.FuncDecl); ok && fn.Body != nil {
				emitBodyEdges(fn.Body, d.id, ti, modSet)
			}
		}
	}
}
