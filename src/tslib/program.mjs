// apg unified JS/TS scanner frontend — TypeScript program host.
//
// Plain ESM sidecar (the `identity.mjs` precedent). Owns the custom compiler
// host that resolves imports between workspace packages directly (package.json
// `name` → package dir), so a fresh checkout scans cleanly without
// `npm install` first. Non-workspace imports fall through to the default
// resolution (node_modules / package.json exports+main). The JavaScript
// extensions are accepted as workspace candidates too, so a JS-only workspace
// package resolves without a build step.

import fs from "node:fs";
import path from "node:path";
import ts from "typescript";
import { ctx } from "./state.mjs";

export function workspaceHost(options) {
  const opts = options;
  const host = opts.incremental
    ? ts.createIncrementalCompilerHost(opts, ts.sys)
    : ts.createCompilerHost(opts);
  const resolveWorkspaceImport = (spec) => {
    for (const pkg of ctx.packages) {
      if (spec === pkg.name || spec.startsWith(pkg.name + "/")) {
        const sub = spec.slice(pkg.name.length + 1);
        const base = path.join(pkg.dir, sub);
        const candidates = [
          base + ".ts",
          base + ".tsx",
          base + ".mts",
          base + ".cts",
          base + ".js",
          base + ".jsx",
          base + ".mjs",
          base + ".cjs",
          base + ".d.ts",
          path.join(base, "index.ts"),
          path.join(base, "index.tsx"),
          path.join(base, "index.js"),
          path.join(base, "index.jsx"),
          path.join(base, "index.mjs"),
          path.join(base, "index.cjs"),
        ];
        for (const c of candidates) {
          if (fs.existsSync(c)) {
            const ext = c.endsWith(".js") ? ts.Extension.Js
              : c.endsWith(".jsx") ? ts.Extension.Jsx
                : c.endsWith(".mjs") ? ts.Extension.Mjs
                  : c.endsWith(".cjs") ? ts.Extension.Cjs
                    : ts.Extension.Ts;
            return { resolvedFileName: c, extension: ext, isExternalLibraryImport: false };
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
