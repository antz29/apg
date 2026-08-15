package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"go/ast"
	"go/printer"
	"go/token"
	"go/types"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"

	"golang.org/x/tools/go/packages"
)

// --- Unified schema messages (SPEC §2) ---

type moduleMsg struct {
	Type string `json:"type"` // module
	Fqn  string `json:"fqn"`
}

type structMsg struct {
	Type   string `json:"type"` // struct
	ID     string `json:"id"`
	Parent string `json:"parent"`
	Name   string `json:"name"`
	Path   string `json:"path"`
	Start  int    `json:"start"`
	End    int    `json:"end"`
}

type funcMsg struct {
	Type   string   `json:"type"` // function
	ID     string   `json:"id"`
	Parent string   `json:"parent"`
	Name   string   `json:"name"`
	Params []string `json:"params"`
	File   string   `json:"file"`
	Path   string   `json:"path"`
	Start  int      `json:"start"`
	End    int      `json:"end"`
}

type unresolvedMsg struct {
	Type     string `json:"type"` // unresolved
	Fqn      string `json:"fqn"`
	Category string `json:"category"`
}

type edgeMsg struct {
	Type       string `json:"type"` // contains | calls | uses | unresolved_call | unresolved_use
	From       string `json:"from"`
	To         string `json:"to"`
	TargetType string `json:"target_type,omitempty"`
}

var enc *json.Encoder

// nextNodeID is the monotonic opaque-id counter (SPEC §3).
var nextNodeID int

func newNodeID() string {
	nextNodeID++
	return fmt.Sprintf("n%d", nextNodeID)
}

// structID / funcID map canonical FQNs (parent.name, or parent.init#file for
// init) to the opaque ids assigned in pass 1, so edge records can reference
// declarations by id in pass 2.
var structID map[string]string
var funcID map[string]string

// unresolvedSeen deduplicates unresolved node records by fqn.
var unresolvedSeen map[string]bool

type moduleInfo struct {
	Path string
	Dir  string
}

// funcKey is the canonical FQN the ingestor will render for a function
// declaration (SPEC §4): parent.name, or parent.init#<basename> for Go init.
func funcKey(parent, name, file string) string {
	if name == "init" {
		return parent + ".init#" + filepath.Base(file)
	}
	return parent + "." + name
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
		Tests: true,
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

	// Emit each module path as a module node.
	emittedPkg := map[string]bool{}
	for _, m := range mods {
		enc.Encode(moduleMsg{Type: "module", Fqn: m.Path})
		emittedPkg[m.Path] = true
	}

	var projectPkgs []*packages.Package
	for _, p := range pkgs {
		if p.TypesInfo == nil {
			continue
		}
		if isProjectPkg(p, root) && !isExcludedPkg(p, excludes) {
			projectPkgs = append(projectPkgs, p)
		}
	}
	sort.Slice(projectPkgs, func(i, j int) bool {
		return projectPkgs[i].PkgPath < projectPkgs[j].PkgPath
	})

	// With Tests:true, go/packages returns test-augmented packages whose
	// Syntax re-includes non-test files alongside *_test.go files. Count and
	// scan each source file exactly once by absolute path.
	seen := map[string]bool{}
	totalFiles := 0
	for _, p := range projectPkgs {
		for _, f := range p.GoFiles {
			if !seen[f] && !isExcluded(f, excludes) {
				seen[f] = true
				totalFiles++
			}
		}
	}

	scanDone := 0
	scanned := map[string]bool{}

	for _, p := range projectPkgs {
		mod := moduleForPkg(p.PkgPath, mods)
		if mod == "" {
			continue
		}
		emitPkgHierarchy(p.PkgPath, mod, enc, emittedPkg)
	}

	// Gather project source files (deduped by absolute path).
	type fileScan struct {
		file     *ast.File
		filePath string
		p        *packages.Package
		decls    []fileDecl
	}
	var scans []fileScan
	for _, p := range projectPkgs {
		for fi, file := range p.Syntax {
			filePath := p.GoFiles[fi]
			if isExcluded(filePath, excludes) || scanned[filePath] {
				continue
			}
			scanned[filePath] = true
			scans = append(scans, fileScan{file: file, filePath: filePath, p: p})
		}
	}

	// Pass 1: assign an opaque id to every declared struct and function, and
	// build the fqn->id maps used by edge records.
	structID = map[string]string{}
	funcID = map[string]string{}
	unresolvedSeen = map[string]bool{}
	for i := range scans {
		scans[i].decls = collectDecls(scans[i].file, scans[i].filePath, scans[i].p)
	}

	// Pass 2: emit node records and edge records.
	for _, s := range scans {
		emitFile(s.file, s.filePath, s.p, s.decls, modSet)
		scanDone++
		fmt.Fprintf(os.Stderr, "\rScanning: %d%% (%d/%d)", scanDone*100/totalFiles, scanDone, totalFiles)
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
								kind:   "function",
								id:     mid,
								parent: parent,
								name:   name.Name,
								params: interfaceMethodParams(m, ti),
								file:   filePath,
								path:   filePath,
								start:  p.Fset.Position(m.Pos()).Offset,
								end:    p.Fset.Position(m.End()).Offset,
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

// emitFile emits node + contains records for the file's declarations and walks
// struct bodies / function bodies for use, call, and unresolved edges.
func emitFile(file *ast.File, filePath string, p *packages.Package, decls []fileDecl, modSet map[string]bool) {
	ti := p.TypesInfo

	for _, d := range decls {
		switch d.kind {
		case "struct":
			enc.Encode(structMsg{
				Type: "struct", ID: d.id, Parent: d.parent, Name: d.name,
				Path: d.path, Start: d.start, End: d.end,
			})
			// module -> struct
			enc.Encode(edgeMsg{Type: "contains", From: d.parent, To: d.id})
			emitStructUses(d, ti, modSet)
		case "function":
			enc.Encode(funcMsg{
				Type: "function", ID: d.id, Parent: d.parent, Name: d.name,
				Params: d.params, File: d.file, Path: d.path, Start: d.start, End: d.end,
			})
			// module or struct -> function
			from := d.parent
			if id, ok := structID[d.parent]; ok {
				from = id
			}
			enc.Encode(edgeMsg{Type: "contains", From: from, To: d.id})
			if fn, ok := d.astNode.(*ast.FuncDecl); ok && fn.Body != nil {
				emitBodyEdges(fn.Body, d.id, ti, modSet)
			}
		}
	}
}

// emitStructUses emits `uses` edges for embedded struct fields and embedded
// interfaces (struct -> struct).
func emitStructUses(d fileDecl, ti *types.Info, modSet map[string]bool) {
	ts, ok := d.astNode.(*ast.TypeSpec)
	if !ok {
		return
	}
	emitUse := func(typ ast.Expr) {
		if tv, ok := ti.Types[typ]; ok && tv.Type != nil {
			if fqn := typeFQN(tv.Type, modSet); fqn != "" {
				if id, ok := structID[fqn]; ok {
					enc.Encode(edgeMsg{Type: "uses", From: d.id, To: id})
				}
			}
		}
	}
	if st, ok := ts.Type.(*ast.StructType); ok {
		for _, field := range st.Fields.List {
			if len(field.Names) == 0 {
				emitUse(field.Type)
			}
		}
	}
	if iface, ok := ts.Type.(*ast.InterfaceType); ok {
		for _, m := range iface.Methods.List {
			if len(m.Names) == 0 {
				emitUse(m.Type)
			}
		}
	}
}

// emitBodyEdges walks a function body, emitting call/use/unresolved edges with
// the enclosing function's id as the source.
func emitBodyEdges(body *ast.BlockStmt, sourceID string, ti *types.Info, modSet map[string]bool) {
	ast.Inspect(body, func(n ast.Node) bool {
		switch node := n.(type) {
		case *ast.CallExpr:
			cls := classifyCall(node, ti, modSet)
			switch cls.kind {
			case "call":
				if id, ok := funcID[cls.target]; ok {
					enc.Encode(edgeMsg{Type: "calls", From: sourceID, To: id})
				}
			case "u_call":
				emitUnresolved(cls.target, cls.category)
				enc.Encode(edgeMsg{Type: "unresolved_call", From: sourceID, To: cls.target, TargetType: cls.targetType})
			case "use":
				if id, ok := structID[cls.target]; ok {
					enc.Encode(edgeMsg{Type: "uses", From: sourceID, To: id})
				}
			case "u_use":
				emitUnresolved(cls.target, cls.category)
				enc.Encode(edgeMsg{Type: "unresolved_use", From: sourceID, To: cls.target})
			}
			return true
		case *ast.CompositeLit:
			if node.Type != nil {
				if tv, ok := ti.Types[node.Type]; ok && tv.Type != nil {
					if fqn := typeFQN(tv.Type, modSet); fqn != "" {
						if id, ok := structID[fqn]; ok {
							enc.Encode(edgeMsg{Type: "uses", From: sourceID, To: id})
						}
					}
				} else {
					emitUnresolved(exprString(node.Type), "unknown")
					enc.Encode(edgeMsg{Type: "unresolved_use", From: sourceID, To: exprString(node.Type)})
				}
			}
		case *ast.TypeAssertExpr:
			if node.Type != nil {
				if tv, ok := ti.Types[node.Type]; ok && tv.Type != nil {
					if fqn := typeFQN(tv.Type, modSet); fqn != "" {
						if id, ok := structID[fqn]; ok {
							enc.Encode(edgeMsg{Type: "uses", From: sourceID, To: id})
						}
					}
				} else {
					emitUnresolved(exprString(node.Type), "unknown")
					enc.Encode(edgeMsg{Type: "unresolved_use", From: sourceID, To: exprString(node.Type)})
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
					if tv, ok := ti.Types[vs.Type]; ok && tv.Type != nil {
						if fqn := typeFQN(tv.Type, modSet); fqn != "" {
							if id, ok := structID[fqn]; ok {
								enc.Encode(edgeMsg{Type: "uses", From: sourceID, To: id})
							}
						}
					} else {
						emitUnresolved(exprString(vs.Type), "unknown")
						enc.Encode(edgeMsg{Type: "unresolved_use", From: sourceID, To: exprString(vs.Type)})
					}
				}
			}
		}
		return true
	})
}

// emitUnresolved emits the unresolved node record for fqn on first encounter
// (records are deduplicated by fqn; the first category wins).
func emitUnresolved(fqn, category string) {
	if fqn == "" || unresolvedSeen[fqn] {
		return
	}
	unresolvedSeen[fqn] = true
	enc.Encode(unresolvedMsg{Type: "unresolved", Fqn: fqn, Category: category})
}

// callClass is the result of classifying a call expression.
type callClass struct {
	kind       string // "call", "u_call", "use", "u_use"
	target     string // edge target FQN (or raw name)
	category   string // UnresolvedTarget.category (u_call / u_use only)
	targetType string // function type of a func-value call (u_call only)
}

// classifyCall resolves a call expression to an edge. Order matters:
// conversions are not calls, then project functions, then builtins, interface
// methods, function-valued variables, and finally external functions.
func classifyCall(call *ast.CallExpr, ti *types.Info, modSet map[string]bool) callClass {
	// Type conversion ([]byte(x), protoimpl.Pointer(x), (*T)(nil), T(x)) —
	// not a function call. Route to a type-use edge instead.
	if tv, ok := ti.Types[call.Fun]; ok && tv.IsType() {
		return classifyConversion(call, tv.Type, ti, modSet)
	}
	// Immediately-invoked function literal: anonymous, not a named function.
	if isIIFE(call.Fun) {
		tt := ""
		if tv, ok := ti.Types[call.Fun]; ok && tv.Type != nil {
			tt = sigString(tv.Type)
		}
		return callClass{"u_call", "func", "func-value", tt}
	}

	obj, isMethodVal := callObject(call, ti)
	if obj == nil {
		return callClass{"u_call", callRawName(call), "unknown", ""}
	}

	switch o := obj.(type) {
	case *types.Func:
		if o.Pkg() != nil && inProjectModules(o.Pkg().Path(), modSet) {
			if fqn := funcFQNAny(o); fqn != "" {
				return callClass{"call", fqn, "", ""}
			}
			// Project method with no resolvable FQN (e.g. method declared on
			// an anonymous interface type) — fall through to unresolved.
		}
		fqn := funcFQNAny(o)
		if fqn == "" {
			if isMethodVal {
				return callClass{"u_call", callRawName(call), "interface-method", ""}
			}
			return callClass{"u_call", callRawName(call), "unknown", ""}
		}
		if o.Pkg() == nil {
			// Universe-scope interface method (e.g. error.Error).
			return callClass{"u_call", fqn, "interface-method", ""}
		}
		return callClass{"u_call", fqn, stdOrExternal(o.Pkg().Path()), ""}
	case *types.Builtin:
		return callClass{"u_call", o.Name(), "builtin", ""}
	case *types.Var:
		if !isFuncType(o.Type()) {
			return callClass{"u_call", callRawName(call), "unknown", ""}
		}
		// Function-valued variable. Package-level vars have a resolvable
		// identity; locals are recorded by bare name.
		if o.Pkg() != nil && o.Parent() == o.Pkg().Scope() {
			return callClass{"u_call", o.Pkg().Path() + "." + o.Name(), "func-value", sigString(o.Type())}
		}
		return callClass{"u_call", o.Name(), "func-value", sigString(o.Type())}
	}
	return callClass{"u_call", callRawName(call), "unknown", ""}
}

// classifyConversion routes a type-conversion call expression to a type-use
// edge: a project type becomes a resolved use, anything else an unresolved use.
func classifyConversion(call *ast.CallExpr, t types.Type, ti *types.Info, modSet map[string]bool) callClass {
	// A named/aliased target gives an exact type identity even when the alias
	// resolves to a builtin underlying type (e.g. protoimpl.Pointer = unsafe.Pointer).
	if obj, _ := callObject(call, ti); obj != nil {
		if tn, ok := obj.(*types.TypeName); ok {
			if tn.Pkg() == nil {
				return callClass{"u_use", exprString(call.Fun), "builtin", ""}
			}
			fqn := tn.Pkg().Path() + "." + tn.Name()
			if inProjectModules(tn.Pkg().Path(), modSet) {
				return callClass{"use", fqn, "", ""}
			}
			return callClass{"u_use", fqn, stdOrExternal(tn.Pkg().Path()), ""}
		}
	}
	// Compound types ([]byte, *T, map[...]...) resolved through the type.
	if fqn := typeFQN(t, modSet); fqn != "" {
		return callClass{"use", fqn, "", ""}
	}
	if fqn := typeFQNAny(t); fqn != "" {
		return callClass{"u_use", fqn, typeCategory(t), ""}
	}
	return callClass{"u_use", exprString(call.Fun), typeCategory(t), ""}
}

// callObject returns the resolved object behind a call's Fun expression,
// unwrapping generic instantiations. isMethodVal reports whether the object
// came from a method-value selection.
func callObject(call *ast.CallExpr, ti *types.Info) (types.Object, bool) {
	switch fun := call.Fun.(type) {
	case *ast.Ident:
		return ti.Uses[fun], false
	case *ast.SelectorExpr:
		if sel, ok := ti.Selections[fun]; ok && sel.Kind() == types.MethodVal {
			return sel.Obj(), true
		}
		return ti.Uses[fun.Sel], false
	case *ast.IndexExpr:
		return callObject(&ast.CallExpr{Fun: fun.X}, ti)
	case *ast.IndexListExpr:
		return callObject(&ast.CallExpr{Fun: fun.X}, ti)
	}
	return nil, false
}

// isIIFE reports whether fun is an immediately-invoked function literal,
// possibly parenthesized.
func isIIFE(fun ast.Expr) bool {
	switch f := fun.(type) {
	case *ast.FuncLit:
		return true
	case *ast.ParenExpr:
		_, ok := f.X.(*ast.FuncLit)
		return ok
	}
	return false
}

func isFuncType(t types.Type) bool {
	if t == nil {
		return false
	}
	_, ok := t.Underlying().(*types.Signature)
	return ok
}

// sigString renders a function type as a compact, package-qualified string.
func sigString(t types.Type) string {
	if t == nil {
		return ""
	}
	return types.TypeString(t, func(p *types.Package) string {
		if p == nil {
			return ""
		}
		return p.Path()
	})
}

// paramStrings renders a signature's parameter types (excluding the receiver),
// package-qualified, in declaration order (SPEC §2.3).
func paramStrings(sig *types.Signature) []string {
	var out []string
	params := sig.Params()
	for i := 0; i < params.Len(); i++ {
		out = append(out, types.TypeString(params.At(i).Type(), func(p *types.Package) string {
			if p == nil {
				return ""
			}
			return p.Path()
		}))
	}
	if out == nil {
		out = []string{}
	}
	return out
}

// funcParams returns the call-signature parameter types of a declared function.
func funcParams(d *ast.FuncDecl, p *packages.Package) []string {
	obj, ok := p.TypesInfo.Defs[d.Name]
	if !ok {
		return []string{}
	}
	fn, ok := obj.(*types.Func)
	if !ok {
		return []string{}
	}
	return paramStrings(fn.Type().(*types.Signature))
}

// interfaceMethodParams returns the parameter types of an interface method
// declaration.
func interfaceMethodParams(m *ast.Field, ti *types.Info) []string {
	if len(m.Names) == 0 {
		return []string{}
	}
	if obj, ok := ti.Defs[m.Names[0]]; ok {
		if fn, ok := obj.(*types.Func); ok {
			return paramStrings(fn.Type().(*types.Signature))
		}
	}
	return []string{}
}

// spanStart is the 0-based start offset of a declaration, including any doc
// comment.
func spanStart(node ast.Node, p *packages.Package) int {
	start := p.Fset.Position(node.Pos()).Offset
	switch d := node.(type) {
	case *ast.GenDecl:
		if d.Doc != nil {
			start = p.Fset.Position(d.Doc.Pos()).Offset
		}
	case *ast.FuncDecl:
		if d.Doc != nil {
			start = p.Fset.Position(d.Doc.Pos()).Offset
		}
	}
	return start
}

// stdOrExternal classifies a package path as stdlib (first segment has no dot)
// or external (first segment is a domain, e.g. github.com, google.golang.org).
func stdOrExternal(pkgPath string) string {
	seg := pkgPath
	if i := strings.IndexByte(seg, '/'); i >= 0 {
		seg = seg[:i]
	}
	if strings.Contains(seg, ".") {
		return "external"
	}
	return "stdlib"
}

// typeCategory classifies a type for an unresolved-use target.
func typeCategory(t types.Type) string {
	switch tt := t.(type) {
	case *types.Named:
		if tt.Obj().Pkg() == nil {
			return "builtin"
		}
		return stdOrExternal(tt.Obj().Pkg().Path())
	case *types.Pointer:
		return typeCategory(tt.Elem())
	case *types.Slice:
		return typeCategory(tt.Elem())
	case *types.Array:
		return typeCategory(tt.Elem())
	case *types.Map:
		return typeCategory(tt.Elem())
	default:
		return "builtin"
	}
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
	case *ast.FuncLit:
		// Immediately-invoked function literal: not a call to a named
		// function. Record a short, stable name instead of dumping the
		// whole function body.
		name = "func"
	case *ast.ParenExpr:
		if _, ok := fun.X.(*ast.FuncLit); ok {
			name = "func"
		} else {
			name = exprString(call.Fun)
		}
	default:
		name = exprString(call.Fun)
	}
	return name
}

func exprString(e ast.Expr) string {
	if e == nil {
		return ""
	}
	var sb strings.Builder
	printer.Fprint(&sb, token.NewFileSet(), e)
	return strings.TrimSpace(sb.String())
}

// funcFQNAny returns the FQN regardless of module membership. Universe-scope
// interface methods (e.g. error.Error) are returned as "error.Error".
func funcFQNAny(obj types.Object) string {
	if obj == nil {
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
		if obj.Pkg() == nil {
			return rn + "." + fn.Name()
		}
		return obj.Pkg().Path() + "." + rn + "." + fn.Name()
	}
	if obj.Pkg() == nil {
		return ""
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

// typeFQNAny returns the FQN of a named type regardless of module membership.
func typeFQNAny(t types.Type) string {
	switch tt := t.(type) {
	case *types.Named:
		obj := tt.Obj()
		if obj.Pkg() == nil {
			return ""
		}
		return obj.Pkg().Path() + "." + obj.Name()
	case *types.Pointer:
		return typeFQNAny(tt.Elem())
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
			enc.Encode(moduleMsg{Type: "module", Fqn: child})
			emitted[child] = true
		}
		enc.Encode(edgeMsg{Type: "contains", From: cur, To: child})
		cur = child
	}
}
