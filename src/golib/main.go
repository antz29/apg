package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"go/ast"
	"go/printer"
	"go/token"
	"go/types"
	"io/fs"
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
	Type      string `json:"type"` // struct
	ID        string `json:"id"`
	Parent    string `json:"parent"`
	Name      string `json:"name"`
	Path      string `json:"path"`
	Start     int    `json:"start"`
	End       int    `json:"end"`
	StartLine int    `json:"start_line"`
	EndLine   int    `json:"end_line"`
}

type funcMsg struct {
	Type      string   `json:"type"` // function
	ID        string   `json:"id"`
	Parent    string   `json:"parent"`
	Name      string   `json:"name"`
	Params    []string `json:"params"`
	File      string   `json:"file"`
	Path      string   `json:"path"`
	Start     int      `json:"start"`
	End       int      `json:"end"`
	StartLine int      `json:"start_line"`
	EndLine   int      `json:"end_line"`
}

type fileMsg struct {
	Type      string `json:"type"` // file
	Path      string `json:"path"`
	Parent    string `json:"parent"`
	StartLine int    `json:"start_line"`
	EndLine   int    `json:"end_line"`
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

// nextNodeID is the monotonic opaque-id counter (SPEC §3). idPrefix
// (`--id-prefix`, default "n") keeps ids unique across frontends when a scan
// merges multiple languages, so `n1` from Go and `n1` from another frontend
// never collide in the shared stream.
var idPrefix = "n"
var nextNodeID int

func newNodeID() string {
	nextNodeID++
	return fmt.Sprintf("%s%d", idPrefix, nextNodeID)
}

// structID / funcID map canonical FQNs (parent.name, or parent.init#file for
// init) to the opaque ids assigned in pass 1, so edge records can reference
// declarations by id in pass 2. They are built over the FULL loaded package
// set (the full resolution context), never over just the emitted packages: a
// resolved cross-package reference must still find its target.
var structID map[string]string
var funcID map[string]string

// emittedID holds the opaque ids whose declaration node records are actually
// part of the emitted stream (the target set, or every package when no filter
// is in force). An edge to a declaration whose id is NOT in this set carries
// the target's canonical FQN instead of the opaque id, so a reference into a
// non-emitted (cached) package survives the ingestor's fact splice; see
// edgeEndpoint.
var emittedID map[string]bool

// edgeEndpoint resolves the `to` endpoint of a calls/uses edge for a target
// declaration with canonical FQN `fqn` and opaque `id`: the opaque id when the
// declaration is part of the emitted stream, else `fqn` itself. The ingestor's
// cached-fact splice resolves a bare FQN against the reused unit, so a
// reference authored by an emitted package to a declaration in a non-emitted
// package is preserved instead of being silently dropped. With no filter in
// force every declaration is emitted, so this is always `id` — byte-identical
// to a full scan.
func edgeEndpoint(fqn, id string) string {
	if emittedID[id] {
		return id
	}
	return fqn
}

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
		fmt.Fprintf(os.Stderr, "Usage: gofrontend <dir> [--module <dir>]... [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]\n")
		os.Exit(1)
	}
	root, _ := filepath.Abs(os.Args[1])

	// Parse --module <dir> pairs, --id-prefix, and the target-set hand-off
	// flags (phase-02 task-10, the pinned interface); remaining args are
	// excludes.
	var moduleDirs []string
	var excludes []string
	var targetsPath, cacheDir, cacheKey string
	args := os.Args[2:]
	for i := 0; i < len(args); i++ {
		switch {
		case args[i] == "--module" && i+1 < len(args):
			moduleDirs = append(moduleDirs, args[i+1])
			i++
		case args[i] == "--id-prefix" && i+1 < len(args):
			idPrefix = args[i+1]
			i++
		case args[i] == "--targets" && i+1 < len(args):
			targetsPath = args[i+1]
			i++
		case args[i] == "--cache-dir" && i+1 < len(args):
			cacheDir = args[i+1]
			i++
		case args[i] == "--cache-key" && i+1 < len(args):
			cacheKey = args[i+1]
			i++
		default:
			excludes = append(excludes, args[i])
		}
	}

	// The native Go build/export cache for this scan lives under the shared
	// store root: <cache-dir>/go/<cache-key>/ (the pinned per-language artifact
	// location, phase-02 task-10). Set it BEFORE any `go` subprocess — module
	// discovery and go/packages both inherit the environment — so unchanged
	// packages' compiled export data is reused across scans instead of being
	// re-type-checked from scratch, and the user's default GOCACHE is untouched.
	if cacheDir != "" {
		if gc, err := applyGoBuildCache(cacheDir, cacheKey); err != nil {
			fmt.Fprintf(os.Stderr, "Warning: could not create GOCACHE %s: %v\n", gc, err)
		}
	}

	// The target set is an EMISSION filter only. The full module graph below is
	// still loaded and type-checked so every reference resolves exactly
	// (global.constraint.frontend-full-context). An absent flag or an empty
	// file means NO filter (the byte-identical full-scan stream).
	var targets fileSet
	if targetsPath != "" {
		ts, err := readTargetSet(targetsPath)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Warning: could not read targets %s: %v\n", targetsPath, err)
		} else if len(ts) > 0 {
			targets = ts
		}
	}

	// Discover modules under root (or restricted to --module dirs).
	mods := discoverModules(root, moduleDirs)
	if len(mods) == 0 {
		fmt.Fprintf(os.Stderr, "No Go modules found under %s; nothing to scan\n", root)
		return
	}
	modSet := map[string]bool{}
	for _, m := range mods {
		modSet[m.Path] = true
	}

	// Load each project module separately. Dir is the module's own directory,
	// so a module nested below the workspace root (e.g. src/golib without a
	// root go.work) resolves its own go.mod, while a root go.work/module is
	// found by walking up.
	var pkgs []*packages.Package
	for _, m := range mods {
		cfg := &packages.Config{
			Mode:  packages.NeedName | packages.NeedFiles | packages.NeedSyntax | packages.NeedTypes | packages.NeedTypesInfo | packages.NeedModule,
			Dir:   m.Dir,
			Tests: true,
		}
		loaded, err := packages.Load(cfg, m.Path+"/...")
		if err != nil {
			fmt.Fprintf(os.Stderr, "Error loading %s: %v\n", m.Path, err)
			continue
		}
		pkgs = append(pkgs, loaded...)
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

	// Module records are GLOBAL scaffolding, emitted for every loaded package
	// regardless of the emission filter: they carry no location and so are not
	// part of any per-file fact unit (the shared store records units per file),
	// and a full scan's module set + Module->Module hierarchy must be present
	// verbatim for the incremental graph to equal a full scan. Only the
	// per-file declaration/reference facts are filtered.
	for _, p := range projectPkgs {
		mod := moduleForPkg(p.PkgPath, mods)
		if mod == "" {
			continue
		}
		emitPkgHierarchy(p.PkgPath, mod, enc, emittedPkg)
	}

	emitFacts(projectPkgs, mods, modSet, targets, excludes)
}

// emitFacts assigns opaque ids over the FULL loaded package set (the full
// resolution context) and emits the per-file declaration/reference facts for
// the packages selected by `targets` (`nil` = every package, the byte-identical
// full-scan stream). The id maps and resolution context are never filtered —
// only the emitted node/declaration stream is — so a resolved reference from an
// emitted package to a declaration in a non-emitted package still resolves.
// Such an edge carries the target's canonical FQN instead of an opaque id
// (edgeEndpoint), which the ingestor's cached-fact splice resolves against the
// reused unit.
func emitFacts(projectPkgs []*packages.Package, mods []moduleInfo, modSet map[string]bool, targets fileSet, excludes []string) {
	// Package-granularity emission selection (phase-02 task-10): a package is
	// re-emitted when any of its Go files is in the target set. `nil` means no
	// filter is in force, so every package is scanned (the full-scan stream).
	targetPkgs := targetPkgPaths(projectPkgs, targets)
	selectedPkg := func(p *packages.Package) bool {
		return targetPkgs == nil || targetPkgs[p.PkgPath]
	}

	// With Tests:true, go/packages returns test-augmented packages whose
	// Syntax re-includes non-test files alongside *_test.go files. Count and
	// scan each source file exactly once by absolute path.
	seen := map[string]bool{}
	totalFiles := 0
	for _, p := range projectPkgs {
		if !selectedPkg(p) {
			continue
		}
		for _, f := range p.GoFiles {
			if !seen[f] && !isEmissionExcluded(f, excludes) {
				seen[f] = true
				totalFiles++
			}
		}
	}

	scanDone := 0
	scanned := map[string]bool{}

	// Pass 1: assign an opaque id to every declared struct and function over the
	// FULL loaded package set, and record which ids belong to the emitted
	// stream. A package that is not re-emitted still contributes ids so that an
	// emitted package's reference to it resolves exactly; its own node records
	// are simply never written (the cached unit supplies them).
	type fileScan struct {
		file     *ast.File
		filePath string
		p        *packages.Package
		decls    []fileDecl
		emit     bool
	}
	structID = map[string]string{}
	funcID = map[string]string{}
	emittedID = map[string]bool{}
	unresolvedSeen = map[string]bool{}

	var scans []fileScan
	for _, p := range projectPkgs {
		emit := selectedPkg(p)
		for fi, file := range p.Syntax {
			filePath := p.GoFiles[fi]
			if isEmissionExcluded(filePath, excludes) || scanned[filePath] {
				continue
			}
			scanned[filePath] = true
			decls := collectDecls(file, filePath, p)
			if emit {
				for _, d := range decls {
					emittedID[d.id] = true
				}
			}
			scans = append(scans, fileScan{file: file, filePath: filePath, p: p, decls: decls, emit: emit})
		}
	}

	// Pass 2: emit node records and edge records for the selected files only.
	for _, s := range scans {
		if !s.emit {
			continue
		}
		emitFile(s.file, s.filePath, s.p, s.decls, modSet)
		// Emit the file node: parent module comes from the package, and the
		// line count from the token.File (a file ending in a newline counts
		// the last line as the line of its final byte).
		f := s.p.Fset.File(s.file.Pos())
		enc.Encode(fileMsg{
			Type: "file", Path: s.filePath, Parent: moduleForPkg(s.p.PkgPath, mods),
			StartLine: 1, EndLine: offsetLine(f, f.Size()-1),
		})
		scanDone++
		fmt.Fprintf(os.Stderr, "\rScanning: %d%% (%d/%d)", scanDone*100/totalFiles, scanDone, totalFiles)
	}
	fmt.Fprintln(os.Stderr)
}

// discoverModules finds the Go modules under root: via `go list -m -json all`
// when a workspace/module sits at root, else by walking the tree for nested
// go.mod files (SPEC 0.9.1 R2 — a repo whose Go code is not at the workspace
// root). Returns modules whose Dir is under root (or under a --module dir).
func discoverModules(root string, moduleDirs []string) []moduleInfo {
	mods := discoverModulesViaGoList(root, moduleDirs)
	if len(mods) == 0 {
		mods = discoverNestedModules(root, moduleDirs)
	}
	return mods
}

// discoverModulesViaGoList runs `go list -m -json all` in root and returns the
// modules whose Dir is under root (or under one of the --module dirs).
func discoverModulesViaGoList(root string, moduleDirs []string) []moduleInfo {
	cmd := exec.Command("go", "list", "-m", "-json", "all")
	cmd.Dir = root
	out, err := cmd.Output()
	if err != nil {
		fmt.Fprintf(os.Stderr, "Warning: go list -m all failed: %v\n", err)
		return nil
	}

	restrict := absRestrict(moduleDirs)

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
		if !strings.HasPrefix(absDir, root) || !dirAllowed(absDir, restrict) {
			continue
		}
		mods = append(mods, moduleInfo{Path: m.Path, Dir: absDir})
	}
	sort.Slice(mods, func(i, j int) bool { return mods[i].Path < mods[j].Path })
	return mods
}

// discoverNestedModules walks root for go.mod files (skipping vendor,
// node_modules, target, and dot dirs) and resolves each module's path with
// `go list -m` from its own directory.
func discoverNestedModules(root string, moduleDirs []string) []moduleInfo {
	restrict := absRestrict(moduleDirs)
	var mods []moduleInfo
	seen := map[string]bool{}
	_ = filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return nil
		}
		if d.IsDir() {
			name := d.Name()
			if name == "vendor" || name == "node_modules" || name == "target" ||
				name == ".git" || strings.HasPrefix(name, ".") {
				return filepath.SkipDir
			}
			return nil
		}
		if d.Name() != "go.mod" {
			return nil
		}
		dir := filepath.Dir(path)
		if !dirAllowed(dir, restrict) || seen[dir] {
			return nil
		}
		cmd := exec.Command("go", "list", "-m")
		cmd.Dir = dir
		out, err := cmd.Output()
		if err != nil {
			fmt.Fprintf(os.Stderr, "Warning: go list -m failed in %s: %v\n", dir, err)
			return nil
		}
		modPath := strings.TrimSpace(string(out))
		if modPath == "" {
			return nil
		}
		seen[dir] = true
		mods = append(mods, moduleInfo{Path: modPath, Dir: dir})
		return nil
	})
	sort.Slice(mods, func(i, j int) bool { return mods[i].Path < mods[j].Path })
	return mods
}

// absRestrict resolves --module dirs to absolute paths.
func absRestrict(moduleDirs []string) []string {
	restrict := make([]string, 0, len(moduleDirs))
	for _, d := range moduleDirs {
		if abs, err := filepath.Abs(d); err == nil {
			restrict = append(restrict, abs)
		}
	}
	return restrict
}

// dirAllowed reports whether absDir falls under one of the restrict dirs (all
// allowed when restrict is empty).
func dirAllowed(absDir string, restrict []string) bool {
	if len(restrict) == 0 {
		return true
	}
	for _, r := range restrict {
		if absDir == r || strings.HasPrefix(absDir, r+string(filepath.Separator)) {
			return true
		}
	}
	return false
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
					enc.Encode(edgeMsg{Type: "uses", From: d.id, To: edgeEndpoint(fqn, id)})
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
					enc.Encode(edgeMsg{Type: "calls", From: sourceID, To: edgeEndpoint(cls.target, id)})
				}
			case "u_call":
				emitUnresolved(cls.target, cls.category)
				enc.Encode(edgeMsg{Type: "unresolved_call", From: sourceID, To: cls.target, TargetType: cls.targetType})
			case "use":
				if id, ok := structID[cls.target]; ok {
					enc.Encode(edgeMsg{Type: "uses", From: sourceID, To: edgeEndpoint(cls.target, id)})
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
							enc.Encode(edgeMsg{Type: "uses", From: sourceID, To: edgeEndpoint(fqn, id)})
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
							enc.Encode(edgeMsg{Type: "uses", From: sourceID, To: edgeEndpoint(fqn, id)})
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
								enc.Encode(edgeMsg{Type: "uses", From: sourceID, To: edgeEndpoint(fqn, id)})
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

// offsetLine returns the 1-based line number containing a 0-based byte offset.
// `end` offsets are exclusive, so callers pass end-1 for the last byte.
func offsetLine(f *token.File, offset int) int {
	if f == nil {
		return 1
	}
	if offset >= f.Size() {
		offset = f.Size() - 1
	}
	if offset < 0 {
		offset = 0
	}
	return f.Line(f.Pos(offset))
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
		if strings.HasPrefix(f, root) && !isToolchainScratch(f) {
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

// --- target-set hand-off (phase-02 task-10, the pinned interface) ---

// fileSet is the set of cleaned absolute source-file paths read from the
// `--targets` hand-off. An empty/nil set means "no emission filter".
type fileSet map[string]bool

// readTargetSet reads the pinned `--targets` list: a UTF-8, newline-delimited
// file of absolute source-file paths, one per line, no header; blank (and
// whitespace-only) lines are ignored. The caller treats a missing flag or an
// empty file as "no filter".
func readTargetSet(path string) (fileSet, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	set := fileSet{}
	sc := bufio.NewScanner(f)
	// Source paths can exceed bufio.Scanner's 64 KiB default; give generous
	// headroom so a long path is never a silent read failure.
	sc.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if line == "" {
			continue
		}
		set[filepath.Clean(line)] = true
	}
	if err := sc.Err(); err != nil {
		return nil, err
	}
	return set, nil
}

// targetPkgPaths maps the target file set onto PACKAGE granularity: the import
// path of every package that contains at least one target file. A package is
// the re-emission unit (its whole file set is re-emitted), so a file shared by
// the plain and test-augmented package variants selects it once. `nil` means
// the target set is absent/empty — no filter, every package is emitted.
func targetPkgPaths(pkgs []*packages.Package, targets fileSet) map[string]bool {
	if len(targets) == 0 {
		return nil
	}
	sel := map[string]bool{}
	for _, p := range pkgs {
		for _, f := range p.GoFiles {
			if targets[filepath.Clean(f)] {
				sel[p.PkgPath] = true
				break
			}
		}
	}
	return sel
}

// goBuildCacheDir is the native Go build/export cache for this scan:
// `<cache-dir>/go/<cache-key>/` — the pinned per-language artifact location.
func goBuildCacheDir(cacheDir, cacheKey string) string {
	dir := filepath.Join(cacheDir, "go")
	if cacheKey != "" {
		dir = filepath.Join(dir, cacheKey)
	}
	return dir
}

// goBuildCacheRoots holds the per-scan GOCACHE directory (in every spelling the
// toolchain might report) once applyGoBuildCache has created it. The emission
// filter consults it so the toolchain's scratch files — notably the generated
// `_testmain` Go writes into the cache when a test package is compiled — never
// surface as scanned project source. Empty until applyGoBuildCache runs.
var goBuildCacheRoots []string

// applyGoBuildCache creates the shared per-scan Go build cache and points
// GOCACHE at it, returning the directory used. Child `go` processes (module
// discovery and go/packages) inherit the environment, so unchanged packages'
// compiled export data is reused across scans. The directory is recorded in
// goBuildCacheRoots so the emission filter keeps the cache — and the generated
// `_testmain` Go writes into it — out of the scanned project source; the cache
// location itself is unchanged, so the cross-scan reuse it exists for is
// preserved.
func applyGoBuildCache(cacheDir, cacheKey string) (string, error) {
	dir := goBuildCacheDir(cacheDir, cacheKey)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return dir, err
	}
	recordGoBuildCacheRoot(dir)
	return dir, os.Setenv("GOCACHE", dir)
}

// recordGoBuildCacheRoot records dir — cleaned, absolute, and
// symlink-resolved — as a toolchain scratch root for the emission filter, so a
// scratch path is recognised whichever spelling go/packages reports it under.
func recordGoBuildCacheRoot(dir string) {
	candidates := []string{filepath.Clean(dir)}
	if abs, err := filepath.Abs(dir); err == nil {
		candidates = append(candidates, abs)
	}
	if resolved, err := filepath.EvalSymlinks(dir); err == nil {
		candidates = append(candidates, resolved)
	}
	seen := map[string]bool{}
	roots := make([]string, 0, len(candidates))
	for _, r := range candidates {
		if r == "" || seen[r] {
			continue
		}
		seen[r] = true
		roots = append(roots, r)
	}
	goBuildCacheRoots = roots
}

// isToolchainScratch reports whether path is Go toolchain scratch relative to
// the per-scan GOCACHE recorded by applyGoBuildCache. With no cache configured
// it still rejects a generated `_testmain.go` basename, the toolchain's
// synthesized test entry point, which is never project source.
func isToolchainScratch(path string) bool {
	if path == "" {
		return false
	}
	if filepath.Base(path) == "_testmain.go" {
		return true
	}
	for _, root := range goBuildCacheRoots {
		if pathAtOrUnder(path, root) {
			return true
		}
	}
	return false
}

// isGeneratedScratchPath is the PURE emission filter behind isToolchainScratch:
// it reports whether path is a Go toolchain scratch artifact for the given
// per-scan GOCACHE directory (`goBuildCacheDir(cacheDir, cacheKey)`) — the
// generated `_testmain` file Go writes into the cache when a test package is
// compiled, or a `_testmain.go` reported elsewhere (e.g. under `$WORK`). It is
// string-only: no filesystem, environment, or process access.
func isGeneratedScratchPath(path, cacheDir string) bool {
	if path == "" {
		return false
	}
	if filepath.Base(path) == "_testmain.go" {
		return true
	}
	return pathAtOrUnder(path, cacheDir)
}

// pathAtOrUnder reports whether path is dir itself or lies beneath it. Both are
// cleaned before comparison, and the separator bound keeps a sibling that
// merely shares a string prefix (…/KEY vs …/KEY-2) out; an empty dir never
// matches.
func pathAtOrUnder(path, dir string) bool {
	if path == "" || dir == "" {
		return false
	}
	path = filepath.Clean(path)
	dir = filepath.Clean(dir)
	if path == dir {
		return true
	}
	return strings.HasPrefix(path, dir+string(filepath.Separator))
}

// isEmissionExcluded reports whether a source path is kept out of the emission
// stream: a user exclude pattern, or a Go toolchain scratch file (the per-scan
// GOCACHE recorded by applyGoBuildCache and the generated `_testmain` in it,
// whose synthesized `init`/`main` must not surface either).
func isEmissionExcluded(path string, excludes []string) bool {
	return isExcluded(path, excludes) || isToolchainScratch(path)
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
