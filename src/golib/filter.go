package main

import (
	"bufio"
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/tools/go/packages"
)

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

// isEmissionExcluded reports whether a source path is kept out of the emission
// stream: a user exclude pattern, or a Go toolchain scratch file (the per-scan
// GOCACHE recorded by applyGoBuildCache and the generated `_testmain` in it,
// whose synthesized `init`/`main` must not surface either).
func isEmissionExcluded(path string, excludes []string) bool {
	return isExcluded(path, excludes) || isToolchainScratch(path)
}
