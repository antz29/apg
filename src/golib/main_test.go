package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"

	"golang.org/x/tools/go/packages"
)

func TestReadTargetSet(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "go.targets")
	// Newline-delimited absolute paths; blank and whitespace-only lines are
	// ignored (the pinned hand-off format).
	body := "/root/a/a.go\n\n   \n/root/b/b.go\n"
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	set, err := readTargetSet(path)
	if err != nil {
		t.Fatalf("readTargetSet: %v", err)
	}
	want := fileSet{"/root/a/a.go": true, "/root/b/b.go": true}
	if len(set) != len(want) {
		t.Fatalf("readTargetSet = %v want %v", set, want)
	}
	for k := range want {
		if !set[k] {
			t.Errorf("missing target %q in %v", k, set)
		}
	}
}

func TestReadTargetSetMissingFile(t *testing.T) {
	if _, err := readTargetSet(filepath.Join(t.TempDir(), "absent.targets")); err == nil {
		t.Fatal("a missing targets file must return an error")
	}
}

func TestReadTargetSetBlankOnlyIsEmpty(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "go.targets")
	if err := os.WriteFile(path, []byte("\n\n   \n"), 0o644); err != nil {
		t.Fatal(err)
	}
	set, err := readTargetSet(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(set) != 0 {
		t.Fatalf("a blank-only targets file must read empty, got %v", set)
	}
	// An empty target set means NO filter: nil selection, every package emitted.
	pkgs := []*packages.Package{{PkgPath: "scratch/a", GoFiles: []string{"/root/a/a.go"}}}
	if sel := targetPkgPaths(pkgs, set); sel != nil {
		t.Fatalf("an empty target set must mean no filter, got %v", sel)
	}
}

func TestTargetPkgPaths(t *testing.T) {
	pkgs := []*packages.Package{
		{PkgPath: "scratch/a", GoFiles: []string{"/root/a/a.go"}},
		{PkgPath: "scratch/b", GoFiles: []string{"/root/b/b.go", "/root/b/b_test.go"}},
		{PkgPath: "scratch/c", GoFiles: []string{"/root/c/c.go"}},
	}

	// A plain file selects its whole package (package granularity).
	sel := targetPkgPaths(pkgs, fileSet{"/root/a/a.go": true})
	if sel == nil || !sel["scratch/a"] || sel["scratch/b"] || sel["scratch/c"] {
		t.Fatalf("plain-file selection = %v", sel)
	}

	// A test file selects its package too: the plain and test-augmented
	// variants share the import path, so both are re-emitted and file claiming
	// stays identical to the full scan.
	sel = targetPkgPaths(pkgs, fileSet{"/root/b/b_test.go": true})
	if sel == nil || !sel["scratch/b"] || sel["scratch/a"] || sel["scratch/c"] {
		t.Fatalf("test-file selection = %v", sel)
	}

	// A non-empty target set that matches no package selects nothing (an
	// explicit filter is in force), never "everything".
	sel = targetPkgPaths(pkgs, fileSet{"/elsewhere/z/z.go": true})
	if sel == nil || len(sel) != 0 {
		t.Fatalf("non-matching selection = %v", sel)
	}
}

func TestGoBuildCacheDir(t *testing.T) {
	if got, want := goBuildCacheDir("/facts", "KEY"), filepath.Join("/facts", "go", "KEY"); got != want {
		t.Fatalf("goBuildCacheDir = %q want %q", got, want)
	}
	if got, want := goBuildCacheDir("/facts", ""), filepath.Join("/facts", "go"); got != want {
		t.Fatalf("goBuildCacheDir (no key) = %q want %q", got, want)
	}
}

func TestApplyGoBuildCache(t *testing.T) {
	t.Setenv("GOCACHE", "original")
	prevRoots := goBuildCacheRoots
	t.Cleanup(func() { goBuildCacheRoots = prevRoots })
	dir := t.TempDir()
	got, err := applyGoBuildCache(dir, "cache-key-token")
	if err != nil {
		t.Fatalf("applyGoBuildCache: %v", err)
	}
	want := filepath.Join(dir, "go", "cache-key-token")
	if got != want {
		t.Fatalf("applyGoBuildCache dir = %q want %q", got, want)
	}
	if fi, err := os.Stat(want); err != nil || !fi.IsDir() {
		t.Fatalf("the GOCACHE dir must exist: %v", err)
	}
	if env := os.Getenv("GOCACHE"); env != want {
		t.Fatalf("GOCACHE = %q want %q", env, want)
	}
	// The per-scan GOCACHE is recorded for the emission filter, so the
	// generated `_testmain` Go writes into it — and the `init`/`main` it
	// synthesizes — never surface as scanned project source.
	if len(goBuildCacheRoots) == 0 {
		t.Fatal("applyGoBuildCache must record the cache root for the emission filter")
	}
	scratch := filepath.Join(want, "53", "0123456789abcdef-d")
	if !isToolchainScratch(scratch) {
		t.Errorf("a file under the recorded GOCACHE (%s) must be toolchain scratch", scratch)
	}
	if isToolchainScratch(filepath.Join(dir, "a.go")) {
		t.Error("project source outside the GOCACHE must not be toolchain scratch")
	}
}

// TestIsGeneratedScratchPath is the phase-03 task-9 unit: the pure emission
// filter rejects the per-scan GOCACHE (dir/key), the generated `_testmain`
// scratch file, and the synthesized `init`/`main` declared in it — while
// keeping ordinary project source.
func TestIsGeneratedScratchPath(t *testing.T) {
	cacheDir := goBuildCacheDir("/facts", "cache-key-token")

	// Go writes the generated test main into the GOCACHE under a content-hash
	// name (no `.go` suffix), so cache membership — not the basename — is the
	// discriminator there.
	generated := filepath.Join(cacheDir, "53", "0123456789abcdef-d")
	for _, p := range []string{
		generated,
		filepath.Join(cacheDir, "_testmain.go"),
		filepath.Join("/tmp", "go-build123", "b001", "_testmain.go"),
	} {
		if !isGeneratedScratchPath(p, cacheDir) {
			t.Errorf("isGeneratedScratchPath(%q) = false, want true (toolchain scratch)", p)
		}
	}

	// The generated test main declares the `init` that registers the tests and
	// the `main` that runs them; both hang off the scratch file, so the same
	// predicate rejects every declaration in it.
	for _, name := range []string{"init", "main"} {
		d := fileDecl{kind: "function", name: name, file: generated}
		if !isGeneratedScratchPath(d.file, cacheDir) {
			t.Errorf("the generated test main's synthesized %s (%q) must be filtered out", name, d.file)
		}
	}

	// Ordinary project source — including real test files — is kept.
	for _, p := range []string{"/repo/a/a.go", "/repo/a/a_test.go"} {
		if isGeneratedScratchPath(p, cacheDir) {
			t.Errorf("isGeneratedScratchPath(%q) = true, want false (project source)", p)
		}
	}

	// A sibling directory that merely shares a string prefix with the cache key
	// is not inside the cache.
	sibling := filepath.Join("/facts", "go", "cache-key-token-2", "53", "x-d")
	if isGeneratedScratchPath(sibling, cacheDir) {
		t.Errorf("isGeneratedScratchPath(%q) = true, want false (cache sibling)", sibling)
	}

	// With no cache dir configured, only the `_testmain.go` basename is
	// rejected (the `$WORK` spelling); ordinary source is untouched.
	if isGeneratedScratchPath("/repo/a/a.go", "") {
		t.Error("an ordinary path with no cache dir must not be filtered")
	}
	if !isGeneratedScratchPath("/repo/a/_testmain.go", "") {
		t.Error("a generated _testmain.go must be filtered even with no cache dir")
	}
}

// TestToolchainScratchFromRecordedCache is the phase-03 task-9 unit for the
// wiring: once applyGoBuildCache has recorded the per-scan GOCACHE
// (`goBuildCacheDir` dir + key), a generated `_testmain` under it never reaches
// the emission stream, while project source does.
func TestToolchainScratchFromRecordedCache(t *testing.T) {
	prev := goBuildCacheRoots
	goBuildCacheRoots = nil
	t.Cleanup(func() { goBuildCacheRoots = prev })

	cacheDir := goBuildCacheDir("/facts", "cache-key-token")
	recordGoBuildCacheRoot(cacheDir)

	scratch := filepath.Join(cacheDir, "53", "0123456789abcdef-d")
	if !isToolchainScratch(scratch) {
		t.Errorf("a path under the recorded GOCACHE (%s) must be scratch", scratch)
	}
	if !isEmissionExcluded(scratch, nil) {
		t.Errorf("the GOCACHE scratch path (%s) must be excluded from emission", scratch)
	}
	if isToolchainScratch("/repo/a/a.go") {
		t.Error("ordinary project source must not be scratch")
	}
	if isEmissionExcluded("/repo/a/a.go", nil) {
		t.Error("ordinary project source must not be excluded from emission")
	}
}

// writeScratchModule writes a small Go module under t.TempDir (a /tmp scratch
// fixture) and returns its symlink-resolved root.
func writeScratchModule(t *testing.T, files map[string]string) string {
	t.Helper()
	root := t.TempDir()
	if resolved, err := filepath.EvalSymlinks(root); err == nil {
		root = resolved
	}
	for rel, body := range files {
		path := filepath.Join(root, filepath.FromSlash(rel))
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return root
}

// scratchFixture is a two-package module: package a declares the struct A and
// the leaf function; package b (the caller) uses/calls them across the package
// boundary.
var scratchFixture = map[string]string{
	"go.mod": "module scratch\n\ngo 1.21\n",
	"a/a.go": "package a\n\ntype A struct{ X int }\n\nfunc Leaf() int { return 1 }\n",
	"b/b.go": "package b\n\nimport \"scratch/a\"\n\nfunc Foo() int {\n\t_ = a.A{X: 1}\n\treturn a.Leaf()\n}\n",
}

// loadScratchProjectPkgs loads the scratch module's project packages under
// `root` (the same project/exclusion filter main applies), sorted by import
// path.
func loadScratchProjectPkgs(t *testing.T, root string) []*packages.Package {
	t.Helper()
	cfg := &packages.Config{
		Mode: packages.NeedName | packages.NeedFiles | packages.NeedSyntax |
			packages.NeedTypes | packages.NeedTypesInfo | packages.NeedModule,
		Dir:   root,
		Tests: true,
	}
	loaded, err := packages.Load(cfg, "scratch/...")
	if err != nil {
		t.Fatalf("packages.Load: %v", err)
	}
	var projectPkgs []*packages.Package
	for _, p := range loaded {
		if p.TypesInfo == nil {
			continue
		}
		if isProjectPkg(p, root) && !isExcludedPkg(p, nil) {
			projectPkgs = append(projectPkgs, p)
		}
	}
	if len(projectPkgs) == 0 {
		t.Fatalf("no project packages loaded under %s", root)
	}
	sort.Slice(projectPkgs, func(i, j int) bool {
		return projectPkgs[i].PkgPath < projectPkgs[j].PkgPath
	})
	return projectPkgs
}

// emitToBuffer resets the scanner's process-global id state, runs emitFacts,
// and returns the raw JSONL stream.
func emitToBuffer(t *testing.T, projectPkgs []*packages.Package, targets fileSet) string {
	t.Helper()
	prevEnc := enc
	prevPrefix, prevNext := idPrefix, nextNodeID
	var buf bytes.Buffer
	enc = json.NewEncoder(&buf)
	idPrefix = "n"
	nextNodeID = 0
	t.Cleanup(func() {
		enc = prevEnc
		idPrefix = prevPrefix
		nextNodeID = prevNext
	})
	mods := []moduleInfo{{Path: "scratch"}}
	emitFacts(projectPkgs, mods, map[string]bool{"scratch": true}, targets, nil)
	return buf.String()
}

// TestEmitFactsCrossPackageEdgeWithTargetFilter is the phase-02 task-10
// regression: when only the CALLING package is re-emitted, a resolved call/use
// into a non-target (cached) package must still be emitted, addressed by the
// target's canonical FQN so the ingestor's fact splice can resolve it. The
// pre-fix code built the opaque-id maps over the filtered package set only and
// silently dropped the edge.
func TestEmitFactsCrossPackageEdgeWithTargetFilter(t *testing.T) {
	root := writeScratchModule(t, scratchFixture)
	projectPkgs := loadScratchProjectPkgs(t, root)

	// Target ONLY package b (the caller); package a is the callee and must be
	// served from the cached unit.
	targetFile := ""
	for _, p := range projectPkgs {
		for _, f := range p.GoFiles {
			if strings.HasSuffix(f, filepath.Join("b", "b.go")) {
				targetFile = f
			}
		}
	}
	if targetFile == "" {
		t.Fatalf("could not locate b/b.go among loaded packages: %d pkgs", len(projectPkgs))
	}

	out := emitToBuffer(t, projectPkgs, fileSet{filepath.Clean(targetFile): true})

	type edge struct{ from, to string }
	idToFqn := map[string]string{}
	emittedFqn := map[string]bool{}
	var calls, uses []edge
	unresolved := map[string]bool{}

	sc := bufio.NewScanner(strings.NewReader(out))
	sc.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if line == "" {
			continue
		}
		var r struct {
			Type   string `json:"type"`
			ID     string `json:"id"`
			Parent string `json:"parent"`
			Name   string `json:"name"`
			Fqn    string `json:"fqn"`
			From   string `json:"from"`
			To     string `json:"to"`
		}
		if err := json.Unmarshal([]byte(line), &r); err != nil {
			t.Fatalf("bad JSONL line %q: %v", line, err)
		}
		switch r.Type {
		case "struct", "function":
			fqn := r.Parent + "." + r.Name
			idToFqn[r.ID] = fqn
			emittedFqn[fqn] = true
		case "calls":
			calls = append(calls, edge{r.From, r.To})
		case "uses":
			uses = append(uses, edge{r.From, r.To})
		case "unresolved":
			unresolved[r.Fqn] = true
		}
	}
	if err := sc.Err(); err != nil {
		t.Fatal(err)
	}

	// The non-target package a must NOT appear in the emitted node stream —
	// that is what makes the endpoint form observable.
	for _, fqn := range []string{"scratch/a.A", "scratch/a.Leaf"} {
		if emittedFqn[fqn] {
			t.Fatalf("non-target package a must not be emitted, but %s is", fqn)
		}
	}

	fooID := ""
	for id, fqn := range idToFqn {
		if fqn == "scratch/b.Foo" {
			fooID = id
		}
	}
	if fooID == "" {
		t.Fatalf("caller scratch/b.Foo was not emitted; nodes=%v", idToFqn)
	}

	// An endpoint is resolvable when it is either an emitted id or already a
	// canonical FQN.
	resolve := func(endpoint string) string {
		if fqn, ok := idToFqn[endpoint]; ok {
			return fqn
		}
		return endpoint
	}
	find := func(edges []edge, want string) (edge, bool) {
		for _, e := range edges {
			if e.from == fooID && resolve(e.to) == want {
				return e, true
			}
		}
		return edge{}, false
	}

	crossCall, ok := find(calls, "scratch/a.Leaf")
	if !ok {
		t.Fatalf("missing cross-package calls edge scratch/b.Foo -> scratch/a.Leaf; calls=%v", calls)
	}
	if crossCall.to != "scratch/a.Leaf" {
		t.Errorf("cross-package call endpoint = %q, want canonical FQN %q (a.Leaf is not emitted)", crossCall.to, "scratch/a.Leaf")
	}

	crossUse, ok := find(uses, "scratch/a.A")
	if !ok {
		t.Fatalf("missing cross-package uses edge scratch/b.Foo -> scratch/a.A; uses=%v", uses)
	}
	if crossUse.to != "scratch/a.A" {
		t.Errorf("cross-package use endpoint = %q, want canonical FQN %q (a.A is not emitted)", crossUse.to, "scratch/a.A")
	}

	if unresolved["scratch/a.Leaf"] || unresolved["scratch/a.A"] {
		t.Errorf("project declarations must resolve, not land as unresolved: %v", unresolved)
	}
}

// TestEmitFactsNoFilterMatchesAllTargets guards the byte-identity property: an
// explicit target set that selects every package must produce exactly the
// no-filter (full-scan) stream. When no filter is in force every declaration is
// emitted, so edge endpoints stay opaque ids and nothing about the stream
// changes.
func TestEmitFactsNoFilterMatchesAllTargets(t *testing.T) {
	root := writeScratchModule(t, scratchFixture)
	projectPkgs := loadScratchProjectPkgs(t, root)

	all := fileSet{}
	for _, p := range projectPkgs {
		for _, f := range p.GoFiles {
			all[filepath.Clean(f)] = true
		}
	}

	noFilter := emitToBuffer(t, projectPkgs, nil)
	allTargets := emitToBuffer(t, projectPkgs, all)
	if noFilter != allTargets {
		t.Errorf("no-filter stream differs from the all-targets stream:\n--- no filter ---\n%s\n--- all targets ---\n%s", noFilter, allTargets)
	}
}
