package main

import (
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
)

type moduleInfo struct {
	Path string
	Dir  string
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
