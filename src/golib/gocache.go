package main

import (
	"os"
	"path/filepath"
	"strings"
)

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
