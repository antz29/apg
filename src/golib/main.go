package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"

	"golang.org/x/tools/go/packages"
)

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
