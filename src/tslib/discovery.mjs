// apg unified JS/TS scanner frontend — package (module) and source discovery.
//
// Plain ESM sidecar (the `identity.mjs` precedent). Owns the "an npm package is
// a module" model: which `package.json` directories become modules and which
// source files belong to each. The repository-relative base and the fallback
// identity come from the pure `packageIdentity` helper, so no synthetic module
// is ever named after the checkout/scan-root basename
// (`requirements.constraint.no-checkout-named-module`).

import fs from "node:fs";
import path from "node:path";
import { packageIdentity } from "./identity.mjs";
import { config } from "./state.mjs";

export function loadPkg(dir) {
  try {
    return JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8"));
  } catch {
    return null;
  }
}

export const isSourceExt = (f) => /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/.test(f);

// true when `dir` contains source files that are NOT under `skipDirs`.
export function hasSourcesOutside(dir, skipDirs) {
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
export function discoverPackages() {
  const packages = [];
  const add = (dir, name) => {
    const abs = path.resolve(dir);
    if (packages.some((p) => p.dir === abs)) return;
    packages.push({ name: name || packageIdentity(repoBase, packages, abs), dir: abs });
  };

  const rootPkg = loadPkg(config.root);

  // The repository-relative base every fallback identity derives from: the git
  // toplevel (walking up for `.git` — a file in a linked worktree), or the scan
  // root when the tree is not a git checkout. Never the checkout/scan-root
  // basename (`requirements.constraint.no-checkout-named-module`).
  const repoBase = (() => {
    let d = config.root;
    for (;;) {
      if (fs.existsSync(path.join(d, ".git"))) return d;
      const parent = path.dirname(d);
      if (parent === d) return config.root;
      d = parent;
    }
  })();

  // The repo-root package's identity: its declared name when it has one,
  // otherwise the repo-relative base rendered by the pure `packageIdentity`
  // helper. A synthetic module named `path.basename(root)` (the checkout /
  // worktree directory) is never minted.
  const rootFallback = (discovered) =>
    rootPkg && typeof rootPkg.name === "string" && rootPkg.name
      ? rootPkg.name
      : packageIdentity(repoBase, discovered, config.root);

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
        const base = path.resolve(config.root, clean);
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
              add(sub, typeof pkg.name === "string" && pkg.name ? pkg.name : packageIdentity(repoBase, packages, sub));
              wsDirs.push(sub);
            }
          }
        } else if (fs.existsSync(base) && fs.statSync(base).isDirectory()) {
          const pkg = loadPkg(base);
          if (pkg) {
            add(base, typeof pkg.name === "string" && pkg.name ? pkg.name : packageIdentity(repoBase, packages, base));
            wsDirs.push(base);
          }
        }
      }
      if (hasSourcesOutside(config.root, wsDirs)) {
        add(config.root, rootFallback(packages));
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
  walk(config.root);

  if (named.length === 0) {
    // No package.json anywhere: one module for the whole scan, identified from
    // the repo-relative base (never the checkout/scan-root basename).
    add(config.root, rootFallback(packages));
    return finish();
  }
  for (const n of named) add(n.dir, n.name);
  if (hasSourcesOutside(config.root, named.map((n) => n.dir))) {
    add(config.root, rootFallback(packages));
  }
  return finish();

  function finish() {
    if (config.moduleDirAbs.length > 0) {
      return packages.filter((p) => config.moduleDirAbs.some((m) => p.dir === m || p.dir.startsWith(m + path.sep)));
    }
    return packages;
  }
}

// ── source collection ────────────────────────────────────────────────
// `nestedPkgDirs` are the OTHER discovered packages: a nested one owns its own
// files, so the walk never descends into it (otherwise a workspace package's
// file would also be re-emitted under the recursive root package —
// `requirements.constraint.no-checkout-named-module`).
export function collectSources(pkgDir, nestedPkgDirs) {
  const files = [];
  const skip = (nestedPkgDirs || []).filter((d) => d !== pkgDir);
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
      if (config.excludes.some((pat) => p.includes(pat))) continue;
      if (e.isDirectory()) {
        if (skip.some((s) => p === s)) continue;
        walk(p);
      } else if (isSourceExt(e.name)) files.push(p);
    }
  };
  walk(pkgDir);
  files.sort();
  return files;
}

// relpath with separators → dots, extension (and trailing `.d`) stripped:
// `src/components/Button.tsx` → `src.components.Button`,
// `src/app.js` → `src.app`.
export function relPrefix(pkgDir, file) {
  const rel = path.relative(pkgDir, file);
  const noExt = rel.replace(/\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/, "").replace(/\.d$/, "");
  return noExt.split(path.sep).join(".");
}
