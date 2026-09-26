package main

import (
	"encoding/json"
	"fmt"
	"go/ast"
	"os"
	"strings"

	"golang.org/x/tools/go/packages"
)

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
