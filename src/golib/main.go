package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"go/ast"
	"go/types"
	"os"
	"os/exec"
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

type moduleInfo struct {
	Path string
	Dir  string
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintf(os.Stderr, "Usage: gofrontend <dir> [--module <dir>]... [exclude...]\n")
		os.Exit(1)
	}
	root, _ := filepath.Abs(os.Args[1])

	// Parse --module <dir> pairs; remaining args are excludes.
	var moduleDirs []string
	var excludes []string
	args := os.Args[2:]
	for i := 0; i < len(args); i++ {
		if args[i] == "--module" && i+1 < len(args) {
			moduleDirs = append(moduleDirs, args[i+1])
			i++
		} else {
			excludes = append(excludes, args[i])
		}
	}

	// Discover modules under root (or restricted to --module dirs).
	mods := discoverModules(root, moduleDirs)
	if len(mods) == 0 {
		fmt.Fprintf(os.Stderr, "Error: no modules discovered under %s\n", root)
		os.Exit(1)
	}
	modSet := map[string]bool{}
	for _, m := range mods {
		modSet[m.Path] = true
	}

	// Load all project modules in one go. Dir=root uses the root go.work,
	// which sidesteps nested-workspace go-version mismatches.
	patterns := make([]string, 0, len(mods))
	for _, m := range mods {
		patterns = append(patterns, m.Path+"/...")
	}
	cfg := &packages.Config{
		Mode:  packages.NeedName | packages.NeedFiles | packages.NeedSyntax | packages.NeedTypes | packages.NeedTypesInfo | packages.NeedModule,
		Dir:   root,
		Tests: false,
	}
	pkgs, err := packages.Load(cfg, patterns...)
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

	// Emit each module path as a top-level pkg node.
	emittedPkg := map[string]bool{}
	for _, m := range mods {
		enc.Encode(pkgMsg{Type: "pkg", Fqn: m.Path})
		emittedPkg[m.Path] = true
	}

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
		mod := moduleForPkg(p.PkgPath, mods)
		if mod == "" {
			continue
		}
		emitPkgHierarchy(p.PkgPath, mod, enc, emittedPkg)
	}

	for _, p := range projectPkgs {
		for fi, file := range p.Syntax {
			filePath := p.GoFiles[fi]
			if isExcluded(filePath, excludes) {
				scanDone++
				continue
			}
			processFile(file, filePath, p, modSet)
			scanDone++
			fmt.Fprintf(os.Stderr, "\rScanning: %d%% (%d/%d)", scanDone*100/totalFiles, scanDone, totalFiles)
		}
	}
	fmt.Fprintln(os.Stderr)
}

// discoverModules runs `go list -m -json all` in root and returns the modules
// whose Dir is under root (or under one of the --module dirs, if given).
func discoverModules(root string, moduleDirs []string) []moduleInfo {
	cmd := exec.Command("go", "list", "-m", "-json", "all")
	cmd.Dir = root
	out, err := cmd.Output()
	if err != nil {
		fmt.Fprintf(os.Stderr, "Warning: go list -m all failed: %v\n", err)
		return nil
	}

	// Restrict to the given module dirs if any were provided.
	restrict := make([]string, 0, len(moduleDirs))
	for _, d := range moduleDirs {
		abs, err := filepath.Abs(d)
		if err != nil {
			continue
		}
		restrict = append(restrict, abs)
	}

	var mods []moduleInfo
	dec := json.NewDecoder(strings.NewReader(string(out)))
	for dec.More() {
		var m struct {
			Path string
			Dir  string
		}
		if err := dec.Decode(&m); err != nil {
			break
		}
		if m.Dir == "" {
			continue
		}
		absDir, err := filepath.Abs(m.Dir)
		if err != nil {
			continue
		}
		if !strings.HasPrefix(absDir, root) {
			continue
		}
		if len(restrict) > 0 {
			ok := false
			for _, r := range restrict {
				if absDir == r || strings.HasPrefix(absDir, r+string(filepath.Separator)) {
					ok = true
					break
				}
			}
			if !ok {
				continue
			}
		}
		mods = append(mods, moduleInfo{Path: m.Path, Dir: absDir})
	}
	sort.Slice(mods, func(i, j int) bool { return mods[i].Path < mods[j].Path })
	return mods
}

// moduleForPkg returns the module path that owns pkgPath (longest prefix match).
func moduleForPkg(pkgPath string, mods []moduleInfo) string {
	best := ""
	for _, m := range mods {
		if pkgPath == m.Path || strings.HasPrefix(pkgPath, m.Path+"/") {
			if len(m.Path) > len(best) {
				best = m.Path
			}
		}
	}
	return best
}

func processFile(file *ast.File, filePath string, p *packages.Package, modSet map[string]bool) {
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
								if fqn := typeFQN(tv.Type, modSet); fqn != "" {
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
								if fqn := typeFQN(tv.Type, modSet); fqn != "" {
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
					target := resolveCall(node, ti, modSet)
					if target != "" {
						enc.Encode(edgeMsg{Type: "call", Source: fullName, Target: target})
						return true
					}
					// External or unresolvable call: record as unresolved so the
					// dependency is not lost.
					ext := resolveCallExternal(node, ti)
					if ext != "" {
						enc.Encode(edgeMsg{Type: "u_call", Source: fullName, Target: ext})
						return true
					}
					enc.Encode(edgeMsg{Type: "u_call", Source: fullName, Target: callRawName(node)})
				case *ast.CompositeLit:
					if node.Type != nil {
						if tv := ti.Types[node.Type]; tv.Type != nil {
							if fqn := typeFQN(tv.Type, modSet); fqn != "" {
								enc.Encode(edgeMsg{Type: "use", Source: fullName, Target: fqn})
							}
						} else {
							enc.Encode(edgeMsg{Type: "u_use", Source: fullName, Target: exprString(node.Type)})
						}
					}
				case *ast.TypeAssertExpr:
					if node.Type != nil {
						if tv := ti.Types[node.Type]; tv.Type != nil {
							if fqn := typeFQN(tv.Type, modSet); fqn != "" {
								enc.Encode(edgeMsg{Type: "use", Source: fullName, Target: fqn})
							}
						} else {
							enc.Encode(edgeMsg{Type: "u_use", Source: fullName, Target: exprString(node.Type)})
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
								if fqn := typeFQN(tv.Type, modSet); fqn != "" {
									enc.Encode(edgeMsg{Type: "use", Source: fullName, Target: fqn})
								}
							} else {
								enc.Encode(edgeMsg{Type: "u_use", Source: fullName, Target: exprString(vs.Type)})
							}
						}
					}
				}
				return true
			})
		}
	}
}

func resolveCall(call *ast.CallExpr, ti *types.Info, modSet map[string]bool) string {
	switch fun := call.Fun.(type) {
	case *ast.Ident:
		if obj, ok := ti.Uses[fun]; ok {
			return funcFQN(obj, modSet)
		}
	case *ast.SelectorExpr:
		if sel, ok := ti.Selections[fun]; ok && sel.Kind() == types.MethodVal {
			return funcFQN(sel.Obj(), modSet)
		}
		if obj, ok := ti.Uses[fun.Sel]; ok {
			return funcFQN(obj, modSet)
		}
	case *ast.IndexExpr:
		if id, ok := fun.X.(*ast.Ident); ok {
			if obj, ok := ti.Uses[id]; ok {
				return funcFQN(obj, modSet)
			}
		}
		if sel, ok := fun.X.(*ast.SelectorExpr); ok {
			if obj, ok := ti.Uses[sel.Sel]; ok {
				return funcFQN(obj, modSet)
			}
		}
	}
	return ""
}

// resolveCallExternal returns the FQN of a call that resolves to a function
// outside the project modules (e.g. fmt.Println), or "" if unresolvable.
func resolveCallExternal(call *ast.CallExpr, ti *types.Info) string {
	switch fun := call.Fun.(type) {
	case *ast.Ident:
		if obj, ok := ti.Uses[fun]; ok {
			return funcFQNAny(obj)
		}
	case *ast.SelectorExpr:
		if sel, ok := ti.Selections[fun]; ok && sel.Kind() == types.MethodVal {
			return funcFQNAny(sel.Obj())
		}
		if obj, ok := ti.Uses[fun.Sel]; ok {
			return funcFQNAny(obj)
		}
	case *ast.IndexExpr:
		if id, ok := fun.X.(*ast.Ident); ok {
			if obj, ok := ti.Uses[id]; ok {
				return funcFQNAny(obj)
			}
		}
		if sel, ok := fun.X.(*ast.SelectorExpr); ok {
			if obj, ok := ti.Uses[sel.Sel]; ok {
				return funcFQNAny(obj)
			}
		}
	}
	return ""
}

func callRawName(call *ast.CallExpr) string {
	var name string
	switch fun := call.Fun.(type) {
	case *ast.Ident:
		name = fun.Name
	case *ast.SelectorExpr:
		name = fun.Sel.Name
	case *ast.IndexExpr:
		name = callRawName(&ast.CallExpr{Fun: fun.X})
	default:
		name = exprString(call.Fun)
	}
	return sanitizeTarget(name)
}

func exprString(e ast.Expr) string {
	if e == nil {
		return ""
	}
	var sb strings.Builder
	ast.Fprint(&sb, nil, e, nil)
	return sanitizeTarget(strings.TrimSpace(sb.String()))
}

// sanitizeTarget strips characters that would break the CSV/JSON edge output
// (commas, newlines, quotes).
func sanitizeTarget(s string) string {
	var sb strings.Builder
	for _, r := range s {
		switch r {
		case ',', '\n', '\r', '"':
			continue
		default:
			sb.WriteRune(r)
		}
	}
	return sb.String()
}

func funcFQN(obj types.Object, modSet map[string]bool) string {
	if obj == nil || obj.Pkg() == nil {
		return ""
	}
	pkgPath := obj.Pkg().Path()
	if !inProjectModules(pkgPath, modSet) {
		return ""
	}
	return funcFQNAny(obj)
}

// funcFQNAny returns the FQN regardless of module membership.
func funcFQNAny(obj types.Object) string {
	if obj == nil || obj.Pkg() == nil {
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
		return obj.Pkg().Path() + "." + rn + "." + fn.Name()
	}
	return obj.Pkg().Path() + "." + fn.Name()
}

func inProjectModules(pkgPath string, modSet map[string]bool) bool {
	for mod := range modSet {
		if pkgPath == mod || strings.HasPrefix(pkgPath, mod+"/") {
			return true
		}
	}
	return false
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

func typeFQN(t types.Type, modSet map[string]bool) string {
	switch tt := t.(type) {
	case *types.Named:
		obj := tt.Obj()
		if obj.Pkg() == nil {
			return ""
		}
		if !inProjectModules(obj.Pkg().Path(), modSet) {
			return ""
		}
		return obj.Pkg().Path() + "." + obj.Name()
	case *types.Pointer:
		return typeFQN(tt.Elem(), modSet)
	default:
		return ""
	}
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
