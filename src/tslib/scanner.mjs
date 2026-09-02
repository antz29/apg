#!/usr/bin/env node
// apg TypeScript scanner frontend.
//
// Exact-fidelity tier (like the Go/Java/Rust frontends): parses and type-checks
// the project with the official TypeScript compiler API (`ts.createProgram` +
// `checker`), so Calls/Uses edges land on the real declared symbol. Anything
// that is not a project symbol becomes an `unresolved_call` / `unresolved_use`
// edge with a category (stdlib for the bundled `lib.d.ts`, external for
// `node_modules`, unknown otherwise), never a fabricated FQN.
//
// Module model: an npm package is a module. A package.json with `workspaces`
// makes each workspace package its own module (the root is skipped unless it
// carries its own sources); a single package is one module named by its
// package.json `name` (or the directory basename). Every source file under a
// package belongs to that package's module. FQNs are module-prefixed and
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
// Usage: node scanner.mjs <dir> [--module <dir>]... [exclude...]
//   node_modules is always skipped (dependency code, like Go's module cache);
//   --module restricts scanning to the given package dirs; remaining args are
//   substring path excludes.

import fs from "node:fs";
import path from "node:path";
import ts from "typescript";

const args = process.argv.slice(2);
if (args.length < 1) {
  console.error("Usage: tsfrontend <dir> [--module <dir>]... [exclude...]");
  process.exit(1);
}
const rootArg = args[0];
let moduleDirs = [];
let excludes = [];
let idPrefix = "n";
for (let i = 1; i < args.length; i++) {
  if (args[i] === "--module" && i + 1 < args.length) {
    moduleDirs.push(args[++i]);
  } else if (args[i] === "--id-prefix" && i + 1 < args.length) {
    idPrefix = args[++i];
  } else {
    excludes.push(args[i]);
  }
}
const root = path.resolve(rootArg);
const moduleDirAbs = moduleDirs.map((d) => path.resolve(path.resolve(root, d === "." ? root : d)));

// ── unified schema emission ──────────────────────────────────────────
const out = process.stdout;
const seenEdges = new Set();
function emitNode(type, fields) {
  out.write(JSON.stringify({ type, ...fields }) + "\n");
}
function emitEdge(type, from, to) {
  // The ingestor dedups edges, but avoid re-emitting identical edges from the
  // broad syntax walk (a class referenced as a value, a type, and a new-target
  // all resolve to the same symbol).
  const k = type + "\0" + from + "\0" + to;
  if (seenEdges.has(k)) return;
  seenEdges.add(k);
  out.write(JSON.stringify({ type, from, to }) + "\n");
}

let nextId = 0;
function newNodeID() {
  return idPrefix + ++nextId;
}

// unresolvedSeen dedups unresolved node records by fqn (first category wins).
const unresolvedSeen = new Set();
function emitUnresolved(fqn, category) {
  if (!fqn || unresolvedSeen.has(fqn)) return;
  unresolvedSeen.add(fqn);
  emitNode("unresolved", { fqn, category });
}

// ── package (module) discovery ───────────────────────────────────────
function loadPkg(dir) {
  try {
    return JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8"));
  } catch {
    return null;
  }
}

const isSourceExt = (f) => /\.(ts|tsx|mts|cts)$/.test(f);

// true when `dir` contains source files that are NOT under `skipDirs`.
function hasSourcesOutside(dir, skipDirs) {
  let found = false;
  const walk = (d) => {
    if (found) return;
    let entries;
    try {
      entries = fs.readdirSync(d, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      if (e.name[0] === "." || e.name === "node_modules") continue;
      const p = path.join(d, e.name);
      if (skipDirs.some((s) => p === s || p.startsWith(s + path.sep))) continue;
      if (e.isDirectory()) walk(p);
      else if (isSourceExt(e.name)) {
        found = true;
        return;
      }
    }
  };
  walk(dir);
  return found;
}

// Returns [{ name, dir }] — the npm packages that become modules.
function discoverPackages() {
  const packages = [];
  const add = (dir, name) => {
    const abs = path.resolve(dir);
    if (packages.some((p) => p.dir === abs)) return;
    packages.push({ name: name || path.basename(abs), dir: abs });
  };

  const rootPkg = loadPkg(root);

  // A root package.json with `workspaces` names the module boundary (each
  // workspace package is a module; the root is skipped unless it carries its
  // own sources).
  if (rootPkg) {
    const ws = rootPkg.workspaces;
    let patterns = [];
    if (Array.isArray(ws)) patterns = ws;
    else if (ws && Array.isArray(ws.packages)) patterns = ws.packages;

    if (patterns.length > 0) {
      const wsDirs = [];
      for (const pat of patterns) {
        const clean = pat.replace(/\/\*{1,2}$/, "");
        const base = path.resolve(root, clean);
        if (pat.endsWith("*") || pat.endsWith("/*")) {
          let subs = [];
          try {
            subs = fs.readdirSync(base, { withFileTypes: true });
          } catch {
            continue;
          }
          for (const e of subs) {
            if (!e.isDirectory()) continue;
            const sub = path.join(base, e.name);
            const pkg = loadPkg(sub);
            if (pkg) {
              add(sub, typeof pkg.name === "string" && pkg.name ? pkg.name : e.name);
              wsDirs.push(sub);
            }
          }
        } else if (fs.existsSync(base) && fs.statSync(base).isDirectory()) {
          const pkg = loadPkg(base);
          if (pkg) {
            add(base, typeof pkg.name === "string" && pkg.name ? pkg.name : path.basename(base));
            wsDirs.push(base);
          }
        }
      }
      if (hasSourcesOutside(root, wsDirs)) {
        add(root, typeof rootPkg.name === "string" && rootPkg.name ? rootPkg.name : path.basename(root));
      }
      return finish();
    }
  }

  // Otherwise discover every named package under the root: a root
  // package.json, or nested ones (a repo whose TS lives in a subdirectory,
  // e.g. a sidecar next to a Go tree). Each `name`d package is a module; the
  // root is a module only if it carries sources outside all of them.
  const named = [];
  const walk = (d) => {
    let entries;
    try {
      entries = fs.readdirSync(d, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      if (e.name[0] === "." || e.name === "node_modules") continue;
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.name === "package.json") {
        const pkg = loadPkg(d);
        if (pkg && typeof pkg.name === "string" && pkg.name) {
          named.push({ dir: d, name: pkg.name });
          return; // a package's own sub-packages stay inside it
        }
      }
    }
  };
  walk(root);

  if (named.length === 0) {
    // No package.json anywhere: one module named by the directory.
    add(root, rootPkg && typeof rootPkg.name === "string" && rootPkg.name ? rootPkg.name : path.basename(root));
    return finish();
  }
  for (const n of named) add(n.dir, n.name);
  if (hasSourcesOutside(root, named.map((n) => n.dir))) {
    add(root, rootPkg && typeof rootPkg.name === "string" && rootPkg.name ? rootPkg.name : path.basename(root));
  }
  return finish();

  function finish() {
    if (moduleDirAbs.length > 0) {
      return packages.filter((p) => moduleDirAbs.some((m) => p.dir === m || p.dir.startsWith(m + path.sep)));
    }
    return packages;
  }
}

// ── source collection ────────────────────────────────────────────────
function collectSources(pkgDir) {
  const files = [];
  const walk = (d) => {
    let entries;
    try {
      entries = fs.readdirSync(d, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      if (e.name[0] === "." || e.name === "node_modules") continue;
      const p = path.join(d, e.name);
      if (excludes.some((pat) => p.includes(pat))) continue;
      if (e.isDirectory()) walk(p);
      else if (isSourceExt(e.name)) files.push(p);
    }
  };
  walk(pkgDir);
  files.sort();
  return files;
}

// relpath with separators → dots, extension (and trailing `.d`) stripped:
// `src/components/Button.tsx` → `src.components.Button`.
function relPrefix(pkgDir, file) {
  const rel = path.relative(pkgDir, file);
  const noExt = rel.replace(/\.(ts|tsx|mts|cts)$/, "").replace(/\.d$/, "");
  return noExt.split(path.sep).join(".");
}

// ── main: packages → sources → program ───────────────────────────────
const packages = discoverPackages();
if (packages.length === 0) {
  console.error(`Error: no TypeScript packages found under ${root}`);
  process.exit(1);
}

let allFiles = [];
for (const pkg of packages) {
  for (const f of collectSources(pkg.dir)) {
    allFiles.push({ file: f, pkg });
  }
}
if (allFiles.length === 0) {
  console.error(`Error: no TypeScript source files found under ${root}`);
  process.exit(1);
}

const programCompilerOptions = {
  target: ts.ScriptTarget.ESNext,
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler,
  allowJs: false,
  jsx: ts.JsxEmit.Preserve,
  strict: false,
  skipLibCheck: true,
  noEmit: true,
};

// Custom compiler host that resolves imports between workspace packages
// directly (package.json `name` → package dir), so a fresh checkout scans
// cleanly without `npm install` first. Non-workspace imports fall through to
// the default resolution (node_modules).
function workspaceHost() {
  const host = ts.createCompilerHost(programCompilerOptions);
  const resolveWorkspaceImport = (spec) => {
    for (const pkg of packages) {
      if (spec === pkg.name || spec.startsWith(pkg.name + "/")) {
        const sub = spec.slice(pkg.name.length + 1);
        const base = path.join(pkg.dir, sub);
        const candidates = [
          base + ".ts",
          base + ".tsx",
          base + ".mts",
          base + ".cts",
          path.join(base, "index.ts"),
          path.join(base, "index.tsx"),
        ];
        for (const c of candidates) {
          if (fs.existsSync(c)) {
            return { resolvedFileName: c, extension: ts.Extension.Ts, isExternalLibraryImport: false };
          }
        }
      }
    }
    return undefined;
  };
  host.resolveModuleNames = (moduleNames, containingFile, reusedNames, redirectedReference, options) =>
    moduleNames.map((m) => {
      const ws = resolveWorkspaceImport(m);
      if (ws) return ws;
      const r = ts.resolveModuleName(m, containingFile, options, host);
      return r.resolvedModule;
    });
  return host;
}

const program = ts.createProgram(allFiles.map((f) => f.file), programCompilerOptions, workspaceHost());
const checker = program.getTypeChecker();
const sourceFiles = new Map(); // abs path → SourceFile (project files only)
for (const f of allFiles) {
  const sf = program.getSourceFile(f.file);
  if (sf) sourceFiles.set(f.file, sf);
}

// ── declaration collection ───────────────────────────────────────────
// Structs keyed by FQN (parent.name); functions keyed by `parent.name(params)`
// so overloads stay distinct (the ingestor renders the FQN from parent/name/
// params). Ids are assigned after sorting by (path, start) for determinism.
const structs = new Map(); // fqn → decl
const funcs = new Map(); // key → decl
const structOrder = [];
const funcOrder = [];
const idByFqn = new Map(); // struct fqn → id
const ctorIdByParent = new Map(); // struct fqn → constructor id
const declIdByKey = new Map(); // `${path}@${start}` → id (all declared nodes)
const structFqnByKey = new Map(); // `${path}@${start}` → struct fqn
const structByParentName = new Map(); // `${parent}\0${name}` → fqn (dedupe merging)

function lineOf(sf, pos) {
  if (pos < 0) return 1;
  return sf.getLineAndCharacterOfPosition(pos).line + 1;
}
function lineEndOf(sf, end) {
  return end <= 0 ? 1 : lineOf(sf, end - 1);
}

// 0-based start of a declaration including a leading JSDoc comment (matches
// the Java frontend, which includes doc comments in spans).
function docStart(node, sf) {
  let start = node.getStart(sf);
  const docs = ts.getJSDocCommentsAndTags(node);
  if (docs && docs.length > 0) {
    const dstart = docs[0].getStart(sf);
    if (dstart < start) start = dstart;
  }
  return start;
}

function keyOf(sf, node) {
  return sf.fileName + "@" + docStart(node, sf);
}

function registerStruct(parent, name, node, sf) {
  const fqn = parent + "." + name;
  const dup = parent + "\0" + name;
  if (structByParentName.has(dup)) return ""; // declaration merging — first wins
  structByParentName.set(dup, fqn);
  const start = docStart(node, sf);
  const end = node.end;
  const d = {
    id: "",
    parent,
    name,
    path: sf.fileName,
    start,
    end,
    sl: lineOf(sf, start),
    el: lineEndOf(sf, end),
    decl: node,
  };
  structs.set(fqn, d);
  structOrder.push(fqn);
  declIdByKey.set(keyOf(sf, node), ""); // id assigned later
  structFqnByKey.set(keyOf(sf, node), fqn);
  return fqn;
}

function registerFunction(parent, name, params, node, sf) {
  const key = parent + "." + name + "(" + params.join(",") + ")";
  if (funcs.has(key)) return ""; // exact duplicate (getter/setter are distinct)
  const start = docStart(node, sf);
  const end = node.end;
  const d = {
    id: "",
    parent,
    name,
    params,
    path: sf.fileName,
    file: sf.fileName,
    start,
    end,
    sl: lineOf(sf, start),
    el: lineEndOf(sf, end),
    decl: node,
  };
  funcs.set(key, d);
  funcOrder.push(key);
  declIdByKey.set(keyOf(sf, node), ""); // id assigned later
  return key;
}

function paramTypes(node, sf) {
  const ps = [];
  if (!node.parameters) return ps;
  for (const p of node.parameters) {
    ps.push(p.type ? p.type.getText(sf).trim() : "any");
  }
  return ps;
}

// The declaration of a symbol that becomes a node: for an overloaded function
// the implementation signature (last, has a body), else the first declaration.
function primaryDecl(sym) {
  if (!sym || !sym.declarations || sym.declarations.length === 0) return null;
  for (let i = sym.declarations.length - 1; i >= 0; i--) {
    const d = sym.declarations[i];
    if (
      ts.isMethodDeclaration(d) ||
      ts.isConstructorDeclaration(d) ||
      ts.isFunctionDeclaration(d) ||
      ts.isGetAccessor(d) ||
      ts.isSetAccessor(d)
    ) {
      if (d.body) return d;
    }
  }
  return sym.declarations[0];
}

// Collects declarations in one source file. `container` is the current
// enclosing-scope parent FQN (starts at the file's `pkg.relpath` prefix).
function collectFile(sf, pkgDir, modFqn) {
  const fileParent = modFqn + "." + relPrefix(pkgDir, sf.fileName);

  // Function-valued variables (`const foo = () => {}`) are declared functions.
  const registerVariableFunctions = (node, parent) => {
    const decls = node.declarationList ? node.declarationList.declarations : [];
    for (const decl of decls) {
      if (!ts.isVariableDeclaration(decl)) continue;
      const init = decl.initializer;
      let fn = null;
      if (init && (ts.isArrowFunction(init) || ts.isFunctionExpression(init))) fn = init;
      else if (init && ts.isParenthesizedExpression(init) && (ts.isArrowFunction(init.expression) || ts.isFunctionExpression(init.expression))) fn = init.expression;
      if (fn && ts.isIdentifier(decl.name)) {
        registerFunction(parent, decl.name.text, paramTypes(fn, sf), decl, sf);
      }
    }
  };

  const walk = (node, parent) => {
    if (!node) return;

    if (ts.isVariableStatement(node)) {
      registerVariableFunctions(node, parent);
      return;
    }

    if (ts.isClassDeclaration(node) || ts.isInterfaceDeclaration(node)) {
      const name = node.name ? node.name.text : null;
      if (name) {
        const fqn = registerStruct(parent, name, node, sf);
        if (fqn) {
          for (const m of node.members) {
            if (ts.isConstructorDeclaration(m)) {
              registerFunction(fqn, "constructor", paramTypes(m, sf), m, sf);
            } else if (ts.isMethodDeclaration(m) || ts.isMethodSignature(m) || ts.isGetAccessor(m) || ts.isSetAccessor(m)) {
              const mname = m.name && ts.isIdentifier(m.name) ? m.name.text : (m.name && ts.isStringLiteral(m.name) ? m.name.text : null);
              if (!mname) continue;
              registerFunction(fqn, mname, paramTypes(m, sf), m, sf);
            } else if (ts.isPropertyDeclaration(m) && m.name && ts.isIdentifier(m.name)) {
              const init = m.initializer;
              let fn = null;
              if (init && (ts.isArrowFunction(init) || ts.isFunctionExpression(init))) fn = init;
              else if (init && ts.isParenthesizedExpression(init) && (ts.isArrowFunction(init.expression) || ts.isFunctionExpression(init.expression))) fn = init.expression;
              if (fn) registerFunction(fqn, m.name.text, paramTypes(fn, sf), m, sf);
            }
          }
        }
      }
      return;
    }

    if (ts.isEnumDeclaration(node) || ts.isTypeAliasDeclaration(node)) {
      if (node.name) registerStruct(parent, node.name.text, node, sf);
      return;
    }

    if (ts.isModuleDeclaration(node)) {
      const name = node.name;
      if (!name || !ts.isIdentifier(name)) return; // ambient external modules are typings
      const fqn = registerStruct(parent, name.text, node, sf);
      const body = node.body;
      if (body && ts.isModuleBlock(body)) {
        for (const s of body.statements) walk(s, fqn);
      }
      return;
    }

    if (ts.isFunctionDeclaration(node)) {
      if (node.name && node.body) {
        registerFunction(parent, node.name.text, paramTypes(node, sf), node, sf);
      }
      return;
    }
  };

  for (const stmt of sf.statements) walk(stmt, fileParent);
}

// Collect declarations.
for (const { file, pkg } of allFiles) {
  const sf = sourceFiles.get(file);
  if (!sf) continue;
  collectFile(sf, pkg.dir, pkg.name);
}

// Assign opaque ids in sorted (path, start) order (deterministic across runs).
const allDecls = [
  ...structOrder.map((fqn) => structs.get(fqn)),
  ...funcOrder.map((key) => funcs.get(key)),
].sort((a, b) => (a.path !== b.path ? (a.path < b.path ? -1 : 1) : a.start - b.start));
for (const d of allDecls) {
  d.id = newNodeID();
  declIdByKey.set(d.path + "@" + d.start, d.id);
  if (structs.get(d.parent + "." + d.name) === d) idByFqn.set(d.parent + "." + d.name, d.id);
  if (d.name === "constructor" && idByFqn.has(d.parent)) ctorIdByParent.set(d.parent, d.id);
}

// Emit module + file records.
for (const pkg of packages) emitNode("module", { fqn: pkg.name });
for (const { file, pkg } of allFiles) {
  const sf = sourceFiles.get(file);
  if (!sf) continue;
  emitNode("file", {
    path: file,
    parent: pkg.name,
    start_line: 1,
    end_line: sf.getLineStarts().length,
  });
}

// Emit node records.
for (const fqn of structOrder) {
  const d = structs.get(fqn);
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
  const parentId = idByFqn.get(d.parent);
  if (parentId) emitEdge("contains", parentId, d.id);
}

// ── pass 2: per-file edge walk ───────────────────────────────────────
function idForDecl(decl) {
  if (!decl) return "";
  const sf = decl.getSourceFile();
  return declIdByKey.get(sf.fileName + "@" + docStart(decl, sf)) || "";
}
function structFqnOfDecl(decl) {
  if (!decl) return "";
  const sf = decl.getSourceFile();
  return structFqnByKey.get(sf.fileName + "@" + docStart(decl, sf)) || "";
}
function categoryOfDecl(decl) {
  if (!decl) return "unknown";
  const f = decl.getSourceFile().fileName;
  if (f.includes("/typescript/lib/")) return "stdlib";
  if (f.includes("/node_modules/")) return "external";
  return "unknown";
}
function unwrapAlias(sym) {
  let s = sym;
  for (let i = 0; i < 8 && s && s.flags & ts.SymbolFlags.Alias; i++) {
    s = checker.getAliasedSymbol(s);
  }
  return s;
}

// The name node to resolve for a call/new expression's callee.
function nameLocation(expr) {
  if (ts.isIdentifier(expr)) return expr;
  if (ts.isPropertyAccessExpression(expr)) return expr.name;
  if (ts.isParenthesizedExpression(expr)) return nameLocation(expr.expression);
  if (ts.isCallExpression(expr) || ts.isNewExpression(expr)) return nameLocation(expr.expression);
  if (ts.isNonNullExpression(expr)) return nameLocation(expr.expression);
  return null;
}

function calleeText(expr, sf) {
  if (!expr) return "?";
  const t = expr.getText(sf).replace(/\s+/g, " ");
  return t.length > 64 ? t.slice(0, 64) + "…" : t;
}

// A declaration node that is callable (emits a Calls edge target).
function isCallableDecl(decl) {
  return (
    ts.isFunctionDeclaration(decl) ||
    ts.isMethodDeclaration(decl) ||
    ts.isMethodSignature(decl) ||
    ts.isConstructorDeclaration(decl) ||
    ts.isGetAccessor(decl) ||
    ts.isSetAccessor(decl) ||
    ts.isVariableDeclaration(decl) // function-valued variable (`const f = () => …`)
  );
}

function handleCall(node, sf, cur) {
  const loc = nameLocation(node.expression);
  if (!loc) {
    emitUnresolved(calleeText(node.expression, sf), "func-value");
    emitEdge("unresolved_call", cur, calleeText(node.expression, sf));
    return;
  }
  const sym = checker.getSymbolAtLocation(loc);
  const resolved = sym ? unwrapAlias(sym) : null;
  const decl = resolved ? primaryDecl(resolved) : null;
  const tid = decl ? idForDecl(decl) : "";
  if (tid) {
    // A class invoked without `new` (sloppy-mode constructor call) is a use of
    // the class, not a call to a Function node (calls edges must target
    // Functions).
    if (isCallableDecl(decl)) emitEdge("calls", cur, tid);
    else emitEdge("uses", cur, tid);
    return;
  }
  if (resolved) {
    const target = calleeText(node.expression, sf);
    if (!target) return;
    emitUnresolved(target, categoryOfDecl(decl));
    emitEdge("unresolved_call", cur, target);
    return;
  }
  emitUnresolved(calleeText(node.expression, sf), "unknown");
  emitEdge("unresolved_call", cur, calleeText(node.expression, sf));
}

function handleNew(node, sf, cur) {
  const loc = nameLocation(node.expression);
  const sym = loc ? checker.getSymbolAtLocation(loc) : null;
  const resolved = sym ? unwrapAlias(sym) : null;
  const decl = resolved ? primaryDecl(resolved) : null;
  const classFqn = decl ? structFqnOfDecl(decl) : "";
  if (classFqn) {
    // A class with an explicit constructor is a call to it; otherwise the new
    // is a type instantiation → uses edge to the class.
    const ctorId = ctorIdByParent.get(classFqn);
    if (ctorId) emitEdge("calls", cur, ctorId);
    else {
      const id = idByFqn.get(classFqn);
      if (id) emitEdge("uses", cur, id);
    }
    return;
  }
  if (resolved) {
    const target = calleeText(node.expression, sf);
    if (!target) return;
    emitUnresolved(target, categoryOfDecl(decl));
    emitEdge("unresolved_use", cur, target);
    return;
  }
  emitUnresolved(calleeText(node.expression, sf), "unknown");
  emitEdge("unresolved_use", cur, calleeText(node.expression, sf));
}

// The base name node of a type reference (unwraps generics/qualified names).
function typeNameLocation(node) {
  if (ts.isTypeReferenceNode(node)) return typeNameLocation(node.typeName);
  if (ts.isArrayTypeNode(node)) return typeNameLocation(node.elementType);
  if (ts.isTypeQueryNode(node)) return typeNameLocation(node.exprName);
  if (ts.isExpressionWithTypeArguments(node)) return typeNameLocation(node.expression);
  if (ts.isQualifiedName(node)) return typeNameLocation(node.right);
  if (ts.isIdentifier(node)) return node;
  return null;
}

function handleType(node, sf, cur) {
  const loc = typeNameLocation(node);
  if (!loc) return;
  const sym = checker.getSymbolAtLocation(loc);
  const resolved = sym ? unwrapAlias(sym) : null;
  const decl = resolved ? primaryDecl(resolved) : null;
  const tid = decl ? idForDecl(decl) : "";
  if (tid) {
    emitEdge("uses", cur, tid);
    return;
  }
  if (resolved && decl) {
    const target = checker.symbolToString(resolved) || loc.getText(sf).trim();
    if (!target) return;
    emitUnresolved(target, categoryOfDecl(decl));
    emitEdge("unresolved_use", cur, target);
    return;
  }
  const t = loc.getText(sf).trim();
  if (!t) return;
  emitUnresolved(t, "unknown");
  emitEdge("unresolved_use", cur, t);
}

function handleJsx(node, sf, cur) {
  // Component references in TSX (`<Button/>`, `<ui.Button/>`) are uses of the
  // component. Intrinsic elements (`<div/>`) resolve to the JSX namespace and
  // are skipped (no symbol at the location).
  const name = node.tagName;
  const loc = ts.isIdentifier(name) || ts.isPropertyAccessExpression(name) ? name : null;
  if (!loc) return;
  const sym = checker.getSymbolAtLocation(loc);
  const resolved = sym ? unwrapAlias(sym) : null;
  const decl = resolved ? primaryDecl(resolved) : null;
  const tid = decl ? idForDecl(decl) : "";
  if (tid) emitEdge("uses", cur, tid);
}

// A declaration node's own name identifier (skipped as a value-use: `class
// Button {}` uses Button, not the other way around).
function isDeclName(node) {
  return !!node.parent && node.parent.name === node;
}

function isTypeRefNode(node) {
  return (
    ts.isTypeReferenceNode(node) ||
    ts.isTypeQueryNode(node) ||
    ts.isExpressionWithTypeArguments(node) ||
    ts.isArrayTypeNode(node) ||
    ts.isTupleTypeNode(node) ||
    ts.isUnionTypeNode(node) ||
    ts.isIntersectionTypeNode(node) ||
    ts.isParenthesizedTypeNode(node)
  );
}

function walkNode(node, sf, cur) {
  if (!node) return;

  // Recompute cur when entering a declared function or struct-like node.
  if (
    ts.isFunctionDeclaration(node) ||
    ts.isMethodDeclaration(node) ||
    ts.isMethodSignature(node) ||
    ts.isConstructorDeclaration(node) ||
    ts.isGetAccessor(node) ||
    ts.isSetAccessor(node)
  ) {
    const newCur = idForDecl(node) || cur;
    ts.forEachChild(node, (c) => walkNode(c, sf, newCur));
    return;
  }
  if (
    ts.isClassDeclaration(node) ||
    ts.isInterfaceDeclaration(node) ||
    ts.isEnumDeclaration(node) ||
    ts.isTypeAliasDeclaration(node) ||
    ts.isModuleDeclaration(node)
  ) {
    const newCur = idForDecl(node) || cur;
    // extends/implements/type-param constraints attribute to the type itself.
    if (newCur && (ts.isClassDeclaration(node) || ts.isInterfaceDeclaration(node))) {
      if (node.heritageClauses) {
        for (const hc of node.heritageClauses) {
          for (const t of hc.types) handleType(t, sf, newCur);
        }
      }
      if (node.typeParameters) {
        for (const tp of node.typeParameters) {
          if (tp.constraint) handleType(tp.constraint, sf, newCur);
        }
      }
    }
    ts.forEachChild(node, (c) => walkNode(c, sf, newCur));
    return;
  }
  // Function-valued variable declarations: attribute edges in the initializer
  // to the declared function node.
  if (ts.isVariableDeclaration(node)) {
    const newCur = idForDecl(node) || cur;
    ts.forEachChild(node, (c) => walkNode(c, sf, newCur));
    return;
  }

  // Edge extraction for nodes directly in the current context.
  if (cur) {
    if (ts.isCallExpression(node)) handleCall(node, sf, cur);
    else if (ts.isNewExpression(node)) handleNew(node, sf, cur);
    else if (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) handleJsx(node, sf, cur);
    else if (isTypeRefNode(node)) handleType(node, sf, cur);
    else if (ts.isIdentifier(node) && !isDeclName(node)) {
      // A class/enum/namespace used as a value (`let x = Foo`).
      const sym = checker.getSymbolAtLocation(node);
      const resolved = sym ? unwrapAlias(sym) : null;
      if (resolved && resolved.declarations) {
        const decl = resolved.declarations.find(
          (d) => ts.isClassDeclaration(d) || ts.isEnumDeclaration(d) || ts.isTypeAliasDeclaration(d)
        );
        const tid = decl ? idForDecl(decl) : "";
        if (tid) emitEdge("uses", cur, tid);
      }
    }
  }
  ts.forEachChild(node, (c) => walkNode(c, sf, cur));
}

// Edge walk per file. Progress on stderr (captured to apg-frontend.log).
const total = allFiles.length;
let done = 0;
for (const { file } of allFiles) {
  const sf = sourceFiles.get(file);
  if (!sf) continue;
  walkNode(sf, sf, null);
  done++;
  if (done % 50 === 0 || done === total) {
    process.stderr.write(`\rScanning: ${Math.floor((done * 100) / total)}% (${done}/${total})`);
  }
}
process.stderr.write("\n");