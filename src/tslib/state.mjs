// apg unified JS/TS scanner frontend — shared mutable scanner state.
//
// Plain ESM sidecar (the `identity.mjs` precedent: a `.mjs` split keeps
// `tsc`/`@ts-nocheck` and the ts frontend formula intact; a `.ts` sibling would
// be rejected by the build's `tsc` emit form). Every container here is created
// exactly once at module evaluation and shared by reference by the orchestrator
// and the other sidecar modules — nothing re-initialises it.
//
// The program-derived context (`ctx.checker`/`ctx.sourceFiles`/`ctx.allFiles`/
// `ctx.packages`) is injected by the orchestrator after the TypeScript program
// is built: ESM forbids an importer assigning an imported `let`, so they live
// as fields on this shared object.

// ── injected program context ─────────────────────────────────────────
export const ctx = {
  checker: null, // ts.TypeChecker (set after program construction)
  sourceFiles: new Map(), // abs path → SourceFile (project files only)
  allFiles: [], // { file, pkg }
  packages: [], // { name, dir }
};

// ── declaration accumulators ─────────────────────────────────────────
// Structs keyed by FQN (parent.name); functions keyed by `parent.name(params)`
// so overloads stay distinct (the ingestor renders the FQN from parent/name/
// params). Ids are assigned after sorting by (path, start) for determinism.
export const structs = new Map(); // fqn → decl
export const funcs = new Map(); // key → decl
export const structOrder = [];
export const funcOrder = [];
export const idByFqn = new Map(); // struct fqn → id
export const ctorIdByParent = new Map(); // struct fqn → constructor id
export const declIdByKey = new Map(); // `${path}@${start}` → id (all declared nodes)
export const structFqnByKey = new Map(); // `${path}@${start}` → struct fqn
export const structByParentName = new Map(); // `${parent}\0${name}` → fqn (dedupe merging)

// ── emission maps ────────────────────────────────────────────────────
// idToFqn maps every declared id to its canonical FQN; emittedID holds the ids
// whose declaration node records are actually part of the emitted stream (the
// target set, or every declaration when no filter is in force). emitEdge's
// id→FQN fallback depends on both being fully populated before the struct/
// function/contains/edge emission.
export const idToFqn = new Map();
export const emittedID = new Set();

// ── configuration resolved from argv ─────────────────────────────────
export const config = {
  idPrefix: "n",
  root: "",
  excludes: [],
  moduleDirAbs: [],
  targetFiles: null, // Set<string> | null (null = no emission filter)
};
