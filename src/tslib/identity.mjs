// apg unified JS/TS scanner frontend — pure package-identity computation.
//
// Side-effect-free by construction: this module performs no filesystem access,
// starts no process, reads no environment and keeps no mutable state, so it is
// importable directly by `node --test` and by the scanner's own transpile-only
// source. It owns the scanner's module-identity contract — the repo base plus
// the discovered package dirs plus a source path yield the file's module
// identity — so no synthetic module is ever named after the checkout or
// scan-root basename (`requirements.constraint.no-checkout-named-module`).
//
// The scanner's module model is "an npm package is a module": a package.json
// `name` is a package's module identity (or a workspace package's declared
// name), and a source file outside every discovered package falls back to an
// identity derived from the repository-relative base — never `path.basename`.

import path from "node:path";

// The identity of a source file that sits directly in the repo base: the base's
// own repo-relative directory is the empty path, so a stable sentinel derived
// from the base itself stands in. It is never the checkout/scan-root basename.
const REPO_ROOT_IDENTITY = "root";

const SOURCE_EXT = /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/;

/**
 * The module identity a source file is emitted under.
 *
 * - A path inside one of `packages` belongs to that package: the deepest
 *   enclosing package's declared `name` is returned, so a file is attributed to
 *   exactly one module even when packages nest.
 * - Otherwise the identity is derived from `repoBase`: the target's
 *   repo-relative directory, `/`-separated components joined with `.`
 *   (`src/components` -> `src.components`), with the repo base itself rendered
 *   as {@link REPO_ROOT_IDENTITY}. The basename of the checkout or scan root is
 *   never used.
 *
 * `targetPath` may be a source file (its directory is used) or a directory; a
 * source extension decides, so the computation stays pure — no `fs.stat`.
 *
 * @param {string} repoBase absolute repo base (git toplevel, else the scan root)
 * @param {{ dir: string, name: string }[]} packages discovered packages
 * @param {string} targetPath absolute source-file or directory path
 * @returns {string} the module identity
 */
export function packageIdentity(repoBase, packages, targetPath) {
  const base = path.resolve(repoBase);
  const target = path.resolve(targetPath);

  let owner = null;
  for (const pkg of packages || []) {
    const dir = path.resolve(pkg.dir);
    if (target === dir || target.startsWith(dir + path.sep)) {
      if (owner === null || dir.length > owner.dir.length) {
        owner = { dir, name: pkg.name };
      }
    }
  }
  if (owner && typeof owner.name === "string" && owner.name) return owner.name;

  const asFile = SOURCE_EXT.test(target);
  const rel = path.relative(base, asFile ? path.dirname(target) : target);
  // The repo base itself (or a path that would escape it) has no repo-relative
  // directory: it is the root package. A `..` segment is never minted.
  if (!rel || rel === "." || rel === ".." || rel.startsWith(".." + path.sep)) {
    return REPO_ROOT_IDENTITY;
  }
  return rel.split(path.sep).join(".");
}
