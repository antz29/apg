package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"go/ast"
	"go/types"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"golang.org/x/tools/go/packages"
)

type pkgMsg struct {
	Type string `json:"type"`
	Fqn  string `json:"fqn"`
}

type declMsg struct {
	Type  string `json:"type"`
	Kind  string `json:"kind"`
	Fqn   string `json:"fqn"`
	Path  string `json:"path"`
	Start int    `json:"start"`
	End   int    `json:"end"`
}

type edgeMsg struct {
	Type   string `json:"type"`
	Parent string `json:"parent,omitempty"`
	Child  string `json:"child,omitempty"`
	Source string `json:"source,omitempty"`
	Target string `json:"target,omitempty"`
}

var enc *json.Encoder

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintf(os.Stderr, "Usage: gofrontend <dir> [exclude...]\n")
		os.Exit(1)
	}
	root, _ := filepath.Abs(os.Args[1])
	excludes := os.Args[2:]

	cfg := &packages.Config{
		Mode:  packages.NeedName | packages.NeedFiles | packages.NeedSyntax | packages.NeedTypes | packages.NeedTypesInfo,
		Dir:   root,
		Tests: false,
	}
	pkgs, err := packages.Load(cfg, "./...")
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
	if packages.PrintErrors(pkgs) > 0 {
		fmt.Fprintf(os.Stderr, "Warning: some packages had errors\n")
	}

	out := bufio.NewWriter(os.Stdout)
	defer out.Flush()
	enc = json.NewEncoder(out)

	modPath := modPathFor(pkgs, root)
	if modPath == "" {
		modPath = filepath.Base(root)
	}

	enc.Encode(pkgMsg{Type: "pkg", Fqn: modPath})
	emittedPkg := map[string]bool{modPath: true}

	var projectPkgs []*packages.Package
	for _, p := range pkgs {
		if isProjectPkg(p, root) && !isExcludedPkg(p, excludes) {
			projectPkgs = append(projectPkgs, p)
		}
	}
	sort.Slice(projectPkgs, func(i, j int) bool {
		return projectPkgs[i].PkgPath < projectPkgs[j].PkgPath
	})

	totalFiles := 0
	for _, p := range projectPkgs {
		for range p.Syntax {
			totalFiles++
		}
	}

	scanDone := 0

	for _, p := range projectPkgs {
		emitPkgHierarchy(p.PkgPath, modPath, enc, emittedPkg)
	}

	for _, p := range projectPkgs {
		for fi, file := range p.Syntax {
			filePath := p.GoFiles[fi]
			if isExcluded(filePath, excludes) {
				scanDone++
				continue
			}
			processFile(file, filePath, p, modPath)
			scanDone++
			fmt.Fprintf(os.Stderr, "\rScanning: %d%% (%d/%d)", scanDone*100/totalFiles, scanDone, totalFiles)
		}
	}
	fmt.Fprintln(os.Stderr)
}

func processFile(file *ast.File, filePath string, p *packages.Package, modPath string) {
	pkgFqn := p.PkgPath
	ti := p.TypesInfo

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
				fullName := pkgFqn + "." + typeName

				start := p.Fset.Position(d.Pos()).Offset
				if d.Doc != nil {
					start = p.Fset.Position(d.Doc.Pos()).Offset
				}
				end := p.Fset.Position(ts.End()).Offset
				enc.Encode(declMsg{Type: "decl", Kind: "class", Fqn: fullName, Path: filePath, Start: start, End: end})
				enc.Encode(edgeMsg{Type: "contains", Parent: pkgFqn, Child: fullName})

				if st, ok := ts.Type.(*ast.StructType); ok {
					for _, field := range st.Fields.List {
						if len(field.Names) == 0 {
							if tv := ti.Types[field.Type]; tv.Type != nil {
								if fqn := typeFQN(tv.Type, modPath); fqn != "" {
									enc.Encode(edgeMsg{Type: "use", Source: fullName, Target: fqn})
								}
							}
						}
					}
				}
			if iface, ok := ts.Type.(*ast.InterfaceType); ok {
				for _, m := range iface.Methods.List {
					if len(m.Names) == 0 {
						if tv := ti.Types[m.Type]; tv.Type != nil {
							if fqn := typeFQN(tv.Type, modPath); fqn != "" {
								enc.Encode(edgeMsg{Type: "use", Source: fullName, Target: fqn})
							}
						}
					} else {
						for _, name := range m.Names {
							methodFqn := fullName + "." + name.Name
							start := p.Fset.Position(m.Pos()).Offset
							end := p.Fset.Position(m.End()).Offset
							enc.Encode(declMsg{Type: "decl", Kind: "method", Fqn: methodFqn, Path: filePath, Start: start, End: end})
							enc.Encode(edgeMsg{Type: "contains", Parent: fullName, Child: methodFqn})
						}
					}
				}
			}
			}

		case *ast.FuncDecl:
			funcName := d.Name.Name
			if funcName == "_" || funcName == "" {
				continue
			}

			var fullName, parentFqn string
			if d.Recv != nil && len(d.Recv.List) > 0 {
				recvType := recvTypeNameAST(d.Recv.List[0].Type)
				parentFqn = pkgFqn + "." + recvType
				fullName = parentFqn + "." + funcName
			} else {
				parentFqn = pkgFqn
				fullName = pkgFqn + "." + funcName
			}

			start := p.Fset.Position(d.Pos()).Offset
			if d.Doc != nil {
				start = p.Fset.Position(d.Doc.Pos()).Offset
			}
			end := p.Fset.Position(d.End()).Offset

			enc.Encode(declMsg{Type: "decl", Kind: "method", Fqn: fullName, Path: filePath, Start: start, End: end})
			enc.Encode(edgeMsg{Type: "contains", Parent: parentFqn, Child: fullName})

			if d.Body == nil {
				continue
			}
			ast.Inspect(d.Body, func(n ast.Node) bool {
				switch node := n.(type) {
				case *ast.CallExpr:
					target := resolveCall(node, ti, modPath)
					if target != "" {
						enc.Encode(edgeMsg{Type: "call", Source: fullName, Target: target})
						return true
					}
					if tv := ti.Types[node.Fun]; tv.Type != nil {
						if _, isSig := tv.Type.(*types.Signature); !isSig {
							if fqn := typeFQN(tv.Type, modPath); fqn != "" {
								enc.Encode(edgeMsg{Type: "use", Source: fullName, Target: fqn})
							}
						}
					}
				case *ast.CompositeLit:
					if node.Type != nil {
						if tv := ti.Types[node.Type]; tv.Type != nil {
							if fqn := typeFQN(tv.Type, modPath); fqn != "" {
								enc.Encode(edgeMsg{Type: "use", Source: fullName, Target: fqn})
							}
						}
					}
				case *ast.TypeAssertExpr:
					if node.Type != nil {
						if tv := ti.Types[node.Type]; tv.Type != nil {
							if fqn := typeFQN(tv.Type, modPath); fqn != "" {
								enc.Encode(edgeMsg{Type: "use", Source: fullName, Target: fqn})
							}
						}
					}
				case *ast.DeclStmt:
					gd, ok := node.Decl.(*ast.GenDecl)
					if !ok {
						return true
					}
					for _, spec := range gd.Specs {
						vs, ok := spec.(*ast.ValueSpec)
						if !ok {
							continue
						}
						if vs.Type != nil {
							if tv := ti.Types[vs.Type]; tv.Type != nil {
								if fqn := typeFQN(tv.Type, modPath); fqn != "" {
									enc.Encode(edgeMsg{Type: "use", Source: fullName, Target: fqn})
								}
							}
						}
					}
				}
				return true
			})
		}
	}
}

func resolveCall(call *ast.CallExpr, ti *types.Info, modPath string) string {
	switch fun := call.Fun.(type) {
	case *ast.Ident:
		if obj, ok := ti.Uses[fun]; ok {
			return funcFQN(obj, modPath)
		}
	case *ast.SelectorExpr:
		if sel, ok := ti.Selections[fun]; ok && sel.Kind() == types.MethodVal {
			return funcFQN(sel.Obj(), modPath)
		}
		if obj, ok := ti.Uses[fun.Sel]; ok {
			return funcFQN(obj, modPath)
		}
	case *ast.IndexExpr:
		if id, ok := fun.X.(*ast.Ident); ok {
			if obj, ok := ti.Uses[id]; ok {
				return funcFQN(obj, modPath)
			}
		}
		if sel, ok := fun.X.(*ast.SelectorExpr); ok {
			if obj, ok := ti.Uses[sel.Sel]; ok {
				return funcFQN(obj, modPath)
			}
		}
	}
	return ""
}

func funcFQN(obj types.Object, modPath string) string {
	if obj == nil || obj.Pkg() == nil {
		return ""
	}
	pkgPath := obj.Pkg().Path()
	if !strings.HasPrefix(pkgPath, modPath) {
		return ""
	}
	fn, ok := obj.(*types.Func)
	if !ok {
		return ""
	}
	sig := fn.Type().(*types.Signature)
	if sig.Recv() != nil {
		rn := recvTypeName(sig.Recv().Type())
		if rn == "" {
			return ""
		}
		return pkgPath + "." + rn + "." + fn.Name()
	}
	return pkgPath + "." + fn.Name()
}

func recvTypeName(t types.Type) string {
	switch tt := t.(type) {
	case *types.Named:
		return tt.Obj().Name()
	case *types.Pointer:
		return recvTypeName(tt.Elem())
	default:
		return ""
	}
}

func recvTypeNameAST(t ast.Expr) string {
	switch tt := t.(type) {
	case *ast.Ident:
		return tt.Name
	case *ast.StarExpr:
		return recvTypeNameAST(tt.X)
	case *ast.IndexExpr:
		return recvTypeNameAST(tt.X)
	case *ast.IndexListExpr:
		return recvTypeNameAST(tt.X)
	default:
		return ""
	}
}

func typeFQN(t types.Type, modPath string) string {
	switch tt := t.(type) {
	case *types.Named:
		obj := tt.Obj()
		if obj.Pkg() == nil {
			return ""
		}
		if !strings.HasPrefix(obj.Pkg().Path(), modPath) {
			return ""
		}
		return obj.Pkg().Path() + "." + obj.Name()
	case *types.Pointer:
		return typeFQN(tt.Elem(), modPath)
	default:
		return ""
	}
}

func modPathFor(pkgs []*packages.Package, root string) string {
	for _, p := range pkgs {
		if len(p.GoFiles) == 0 {
			continue
		}
		if strings.HasPrefix(p.GoFiles[0], root) && p.Module != nil {
			return p.Module.Path
		}
	}
	// fallback: find package with shortest PkgPath under root
	var best string
	for _, p := range pkgs {
		for _, f := range p.GoFiles {
			if strings.HasPrefix(f, root) {
				if best == "" || len(p.PkgPath) < len(best) {
					best = p.PkgPath
				}
				break
			}
		}
	}
	return best
}

func isProjectPkg(p *packages.Package, root string) bool {
	for _, f := range p.GoFiles {
		if strings.HasPrefix(f, root) {
			return true
		}
	}
	return false
}

func isExcludedPkg(p *packages.Package, excludes []string) bool {
	if len(excludes) == 0 {
		return false
	}
	allExcluded := true
	for _, f := range p.GoFiles {
		if !isExcluded(f, excludes) {
			allExcluded = false
			break
		}
	}
	return allExcluded
}

func isExcluded(path string, excludes []string) bool {
	for _, pat := range excludes {
		if strings.Contains(path, pat) {
			return true
		}
	}
	return false
}

func emitPkgHierarchy(pkgFqn, modPath string, enc *json.Encoder, emitted map[string]bool) {
	if pkgFqn == modPath {
		return
	}
	rel := strings.TrimPrefix(pkgFqn, modPath+"/")
	if rel == pkgFqn {
		return
	}
	parts := strings.Split(rel, "/")
	cur := modPath
	for _, part := range parts {
		if part == "" {
			continue
		}
		child := cur + "/" + part
		if !emitted[child] {
			enc.Encode(pkgMsg{Type: "pkg", Fqn: child})
			emitted[child] = true
		}
		enc.Encode(edgeMsg{Type: "contains", Parent: cur, Child: child})
		cur = child
	}
}
