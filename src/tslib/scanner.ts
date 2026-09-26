#!/usr/bin/env node
// @ts-nocheck
// apg unified JS/TS scanner frontend.
//
// Exact-fidelity tier (like the Go/Java/Rust frontends): parses and type-checks
// the project with the official TypeScript compiler API (`ts.createProgram` +
// `checker`), so Calls/Uses edges land on the real declared symbol. Anything
// that is not a project symbol becomes an `unresolved_call` / `unresolved_use`
// edge with a category (stdlib for the bundled `lib.d.ts`, external for
// `node_modules`, unknown otherwise), never a fabricated FQN.
//
// Unified JS/TS: `.ts/.tsx/.mts/.cts` plus the JavaScript extensions
// `.js/.jsx/.mjs/.cjs` are accepted via `allowJs`, with the compiler's native
// CJS/ESM/package resolution. Dynamic/untyped JavaScript constructs still yield
// UnresolvedTarget only — never a fabricated FQN.
//
// Module model: an npm package is a module. A package.json with `workspaces`
// makes each workspace package its own module (the root is skipped unless it
// carries its own sources); a single package is one module named by its
// package.json `name`. A source file outside every discovered package takes a
// fallback identity derived from the repository-relative base (the git top
// level, else the scan root) — never the checkout/scan-root directory basename
// (`requirements.constraint.no-checkout-named-module`). Every source file under
// a package belongs to that package's module. FQNs are module-prefixed and
// file-path-prefixed (each ES module file is its own namespace), so two files
// in the same package can both declare `class Button` without colliding:
//
//   package `@co/ui`, file `src/components/Button.tsx`, class `Button`:
//       module        @co/ui
//       struct FQN    @co/ui.src.components.Button.Button
//       method FQN    @co/ui.src.components.Button.Button.onClick
//
// start/end are UTF-16 code-unit offsets (TypeScript's native positions), the
// same convention the Java frontend uses (javac's UTF-16 char positions);
// start_line/end_line are 1-based inclusive line numbers.
//
// Usage: node scanner.mjs <dir> [--module <dir>]... [--targets <file>]
//        [--cache-dir <dir>] [--cache-key <key>] [exclude...]
//   node_modules is always skipped (dependency code, like Go's module cache);
//   --module restricts scanning to the given package dirs; remaining args are
//   substring path excludes.
//
// Win-B full-context-targeted emission (the pinned phase-02 task-9 interface):
//   --targets <file>   UTF-8, newline-delimited absolute source-file paths;
//                      absent/empty = no emission filter (the byte-identical
//                      full scan). Every target is still resolved against the
//                      FULL program context; only the target files' facts are
//                      emitted, and a reference into a non-emitted (cached)
//                      file carries that declaration's canonical FQN.
//   --cache-dir <dir>  the shared cache root (<git-common-dir>/apg/facts).
//   --cache-key <key>  the global cache key. The TypeScript compiler's native
//                      incremental build-info is persisted at
//                      <cache-dir>/ts/<cache-key>/tsconfig.tsbuildinfo so a
//                      re-scan (or a fresh worktree with the same binary +
//                      format + config) reuses it; a key mismatch discards it.
//
// The scanner is a thin orchestrator: the cohesive concerns live in plain-ESM
// sidecar modules (the `identity.mjs` precedent — a `.mjs` split keeps
// `tsc`/`@ts-nocheck` and the ts frontend formula intact) imported with explicit
// `./name.mjs` specifiers:
//   state.mjs        shared mutable scanner state (containers only)
//   emit.mjs         unified-schema JSONL node/edge emission
//   discovery.mjs    package/source discovery
//   program.mjs      the TypeScript program/workspace host
//   declarations.mjs declaration extraction
//   resolve.mjs      symbol/type resolution
//   walk.mjs         the AST walk and edge emission

import fs from "node:fs";
import path from "node:path";
import ts from "typescript";
import {
  ctx,
  config,
  structs,
  funcs,
  structOrder,
  funcOrder,
  idByFqn,
  ctorIdByParent,
  declIdByKey,
  idToFqn,
  emittedID,
} from "./state.mjs";
import { emitNode, emitEdge, newNodeID } from "./emit.mjs";
import { discoverPackages, collectSources } from "./discovery.mjs";
import { workspaceHost } from "./program.mjs";
import { collectFile } from "./declarations.mjs";
import { walkNode } from "./walk.mjs";

const args = process.argv.slice(2);
if (args.length < 1) {
  console.error("Usage: tsfrontend <dir> [--module <dir>]... [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]");
  process.exit(1);
}
const rootArg = args[0];
let moduleDirs = [];
let excludes = [];
let idPrefix = "n";
let targetsPath = "";
let cacheDir = "";
let cacheKey = "";
for (let i = 1; i < args.length; i++) {
  if (args[i] === "--module" && i + 1 < args.length) {
    moduleDirs.push(args[++i]);
  } else if (args[i] === "--id-prefix" && i + 1 < args.length) {
    idPrefix = args[++i];
  } else if (args[i] === "--targets" && i + 1 < args.length) {
    targetsPath = args[++i];
  } else if (args[i] === "--cache-dir" && i + 1 < args.length) {
    cacheDir = args[++i];
  } else if (args[i] === "--cache-key" && i + 1 < args.length) {
    cacheKey = args[++i];
  } else {
    excludes.push(args[i]);
  }
}
const root = path.resolve(rootArg);
config.root = root;
config.idPrefix = idPrefix;
config.excludes = excludes;
config.moduleDirAbs = moduleDirs.map((d) => path.resolve(path.resolve(root, d === "." ? root : d)));

// The target set is an EMISSION filter only (global.constraint.frontend-full-
// context). `null` means no filter is in force, so every declaration is
// emitted — the byte-identical full-scan stream. A missing/unreadable file
// degrades to no filter (with a warning), never to an empty scan.
config.targetFiles = null;
if (targetsPath) {
  try {
    const set = new Set();
    for (const line of fs.readFileSync(targetsPath, "utf8").split(/\r?\n/)) {
      const t = line.trim();
      if (t) set.add(path.resolve(t));
    }
    if (set.size > 0) config.targetFiles = set;
  } catch {
    process.stderr.write(`warning: could not read targets ${targetsPath}; scanning unfiltered\n`);
  }
}

// ── main: packages → sources → program ───────────────────────────────
ctx.packages = discoverPackages();
if (ctx.packages.length === 0) {
  console.error(`Error: no TypeScript/JavaScript packages found under ${root}`);
  process.exit(1);
}

for (const pkg of ctx.packages) {
  for (const f of collectSources(pkg.dir, ctx.packages.map((p) => p.dir))) {
    ctx.allFiles.push({ file: f, pkg });
  }
}
if (ctx.allFiles.length === 0) {
  console.error(`Error: no TypeScript/JavaScript source files found under ${root}`);
  process.exit(1);
}

const programCompilerOptions = {
  target: ts.ScriptTarget.ESNext,
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler,
  allowJs: true,
  jsx: ts.JsxEmit.Preserve,
  strict: false,
  skipLibCheck: true,
  noEmit: true,
};

// Build the program over the FULL source set (the full resolution context).
// With a shared cache root the compiler's native incremental engine is used and
// its build-info is persisted under `<cache-dir>/ts/<cache-key>/` so a re-scan
// reuses it; only the build-info file is written (the JS output is suppressed,
// nothing is emitted into the scanned tree).
let program;
if (cacheDir) {
  const tsCacheDir = path.join(cacheDir, "ts", cacheKey || "");
  fs.mkdirSync(tsCacheDir, { recursive: true });
  const tsBuildInfoPath = path.join(tsCacheDir, "tsconfig.tsbuildinfo");
  const incrementalOptions = {
    ...programCompilerOptions,
    noEmit: false,
    incremental: true,
    tsBuildInfoFile: tsBuildInfoPath,
  };
  const oldProgram = ts.readBuilderProgram(incrementalOptions, {
    useCaseSensitiveFileNames: () => ts.sys.useCaseSensitiveFileNames,
    getCurrentDirectory: () => ts.sys.getCurrentDirectory(),
    readFile: (f) => ts.sys.readFile(f),
  });
  const builder = ts.createEmitAndSemanticDiagnosticsBuilderProgram(
    ctx.allFiles.map((f) => f.file),
    incrementalOptions,
    workspaceHost(incrementalOptions),
    oldProgram,
  );
  builder.emit(undefined, (fileName, data) => {
    if (fileName === tsBuildInfoPath) {
      try {
        fs.writeFileSync(fileName, data);
      } catch {
        // A cache write failure is never fatal: the scan continues full-context.
      }
    }
  });
  program = builder.getProgram();
} else {
  program = ts.createProgram(ctx.allFiles.map((f) => f.file), programCompilerOptions, workspaceHost(programCompilerOptions));
}
ctx.checker = program.getTypeChecker();
for (const f of ctx.allFiles) {
  const sf = program.getSourceFile(f.file);
  if (sf) ctx.sourceFiles.set(f.file, sf);
}

// ── declaration collection ───────────────────────────────────────────
// Collect declarations over the FULL file set (the full resolution context).
// The accumulators live in state.mjs; ids are assigned after sorting by
// (path, start) for determinism.
for (const { file, pkg } of ctx.allFiles) {
  const sf = ctx.sourceFiles.get(file);
  if (!sf) continue;
  collectFile(sf, pkg.dir, pkg.name);
}

// Assign opaque ids in sorted (path, start) order (deterministic across runs).
const allDecls = [
  ...structOrder.map((fqn) => structs.get(fqn)),
  ...funcOrder.map((key) => funcs.get(key)),
].sort((a, b) => (a.path !== b.path ? (a.path < b.path ? -1 : 1) : a.start - b.start));

// Overload grouping for the canonical function FQN (SPEC §4): a (parent, name)
// declared more than once renders `parent.name(params)` for every member, a
// singleton renders `parent.name`. This is what the ingestor renders, so a
// cross-target edge carries the exact FQN the splice expects.
const funcGroupCount = new Map();
for (const key of funcOrder) {
  const d = funcs.get(key);
  const g = d.parent + "\0" + d.name;
  funcGroupCount.set(g, (funcGroupCount.get(g) || 0) + 1);
}

for (const d of allDecls) {
  d.id = newNodeID();
  declIdByKey.set(d.path + "@" + d.start, d.id);
  if (structs.get(d.parent + "." + d.name) === d) idByFqn.set(d.parent + "." + d.name, d.id);
  if (d.name === "constructor" && idByFqn.has(d.parent)) ctorIdByParent.set(d.parent, d.id);
}

// Map every id to its canonical FQN and record which ids are part of the
// emitted stream (the target set, or all of them when no filter is in force).
for (const fqn of structOrder) {
  const d = structs.get(fqn);
  idToFqn.set(d.id, fqn);
  if (config.targetFiles === null || config.targetFiles.has(path.resolve(d.path))) emittedID.add(d.id);
}
for (const key of funcOrder) {
  const d = funcs.get(key);
  const fqn = funcGroupCount.get(d.parent + "\0" + d.name) > 1
    ? d.parent + "." + d.name + "(" + d.params.join(",") + ")"
    : d.parent + "." + d.name;
  idToFqn.set(d.id, fqn);
  if (config.targetFiles === null || config.targetFiles.has(path.resolve(d.file))) emittedID.add(d.id);
}

// The files whose per-file facts are emitted (every file with no filter).
const emitFileList = config.targetFiles === null
  ? ctx.allFiles
  : ctx.allFiles.filter(({ file }) => config.targetFiles.has(path.resolve(file)));

// Emit module records (scaffolding — no location, never filtered) and the
// emitted files' file records.
for (const pkg of ctx.packages) emitNode("module", { fqn: pkg.name });
for (const { file, pkg } of emitFileList) {
  const sf = ctx.sourceFiles.get(file);
  if (!sf) continue;
  emitNode("file", {
    path: file,
    parent: pkg.name,
    start_line: 1,
    end_line: sf.getLineStarts().length,
  });
}

// Emit node records (only the emitted declarations).
for (const fqn of structOrder) {
  const d = structs.get(fqn);
  if (!emittedID.has(d.id)) continue;
  emitNode("struct", {
    id: d.id,
    parent: d.parent,
    name: d.name,
    path: d.path,
    start: d.start,
    end: d.end,
    start_line: d.sl,
    end_line: d.el,
  });
}
for (const key of funcOrder) {
  const d = funcs.get(key);
  if (!emittedID.has(d.id)) continue;
  emitNode("function", {
    id: d.id,
    parent: d.parent,
    name: d.name,
    params: d.params,
    file: d.file,
    path: d.path,
    start: d.start,
    end: d.end,
    start_line: d.sl,
    end_line: d.el,
  });
}

// Structural containment: methods hang under their class/interface (SPEC §7).
for (const key of funcOrder) {
  const d = funcs.get(key);
  if (!emittedID.has(d.id)) continue;
  const parentId = idByFqn.get(d.parent);
  if (parentId) emitEdge("contains", parentId, d.id);
}

// ── pass 2: per-file edge walk ───────────────────────────────────────
// Edge walk over the emitted (target) files. Progress on stderr (captured to
// apg-frontend.log).
const total = emitFileList.length;
let done = 0;
for (const { file } of emitFileList) {
  const sf = ctx.sourceFiles.get(file);
  if (!sf) continue;
  walkNode(sf, sf, null);
  done++;
  if (done % 50 === 0 || done === total) {
    process.stderr.write(`\rScanning: ${Math.floor((done * 100) / total)}% (${done}/${total})`);
  }
}
process.stderr.write("\n");
