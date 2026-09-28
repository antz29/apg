// Shared plumbing for the apg tool suite (installed by `apg init` into
// .opencode/lib/apg.ts). Lives outside .opencode/tools/ so opencode does not
// auto-discover it as a tool.
//
// Each suite tool shells out to `apg query` with a curated, fixed Cypher
// template. This module owns the project-root discovery, the subprocess call,
// and Cypher string-literal escaping (so structured args can never break out
// of or inject into a query).

import { existsSync, readFileSync, readdirSync, realpathSync } from "node:fs"
import { homedir } from "node:os"
import path from "node:path"

export interface ToolContext {
  directory: string
  worktree: string
  /**
   * The acting agent's NAME, as carried on opencode's v1 tool context
   * (`@opencode-ai/plugin` `ToolContext.agent`). Typed structurally here so
   * this module never imports the plugin at runtime — the tools receive the
   * real field, the plugin-free unit/e2e tiers simply omit it.
   */
  agent?: string
}

/** The apg binary to spawn: APG_BINARY env override, default "apg" on PATH. */
export function apgBinary(): string {
  return process.env.APG_BINARY || "apg"
}

/** Walks up from the session dirs looking for the project's `apg/.trans/db.lbug`.
 *  In a project worktree this resolves to the worktree's OWN `apg/` (and its
 *  branch DB) — never the main checkout's — so the tools work unchanged with
 *  cwd inside the worktree.
 *
 *  `directory` (the tools' optional arg) is the caller's project directory and
 *  is AUTHORITATIVE: when given, the walk-up starts there and only there, so
 *  the returned project root is the caller's checkout — the base the curated
 *  tools rebase stored repo-relative identities onto
 *  (`requirements.requirement.absolute-paths-at-tool-boundary`) — never a
 *  project the session merely happens to have cwd in. The context/cwd dirs are
 *  fallbacks only when no `directory` is given. */
export function findApgRoot(context: ToolContext, directory?: string): string | null {
  const starts = directory
    ? [directory]
    : [context.directory, process.cwd(), context.worktree]
  for (const s of starts) {
    if (!s) continue
    let dir = s
    while (true) {
      if (existsSync(path.join(dir, "apg", ".trans", "db.lbug"))) return dir
      const parent = path.dirname(dir)
      if (parent === dir) break
      dir = parent
    }
  }
  return null
}

/**
 * The MAIN checkout root for a caller that may sit inside a linked project
 * worktree, plus that worktree's project name.
 *
 * The suite runs with cwd inside `<main>/apg/.worktrees/<project>`, but the
 * write boundary the fs tools enforce is anchored to the MAIN checkout — the
 * repo root the worktree shares its `.git` with — so a tool must resolve
 * `<main>` from whatever directory the caller passed, in ANY string form
 * (relative, trailing slash, through a symlink). `root` is that main checkout;
 * `project` is the linked worktree's name `<project>`, or `null` when the
 * caller is in the main checkout itself.
 *
 * Resolution is canonical-first and path-first: the caller's directory is made
 * absolute and symlink-resolved, then the documented
 * `<main>/apg/.worktrees/<project>` layout (`apg project start`'s worktree
 * home) decodes straight to `<main>` + `<project>`. When no such layout is
 * present — the main checkout, or a worktree at another path — git's own layout
 * decides: a linked worktree's git dir is `<main>/.git/worktrees/<project>`
 * (whose grandparent is the main root), the main checkout's is `<main>/.git`.
 * Returns `null` when nothing resolves (no git repository).
 *
 * Real I/O (fs + git), so it is covered by the opt-in suite e2e, never the
 * side-effect-free unit tier; the PURE `fsScopeDecision` takes the resolved
 * root as its frame and stays pure.
 */
export async function mainCheckoutRoot(
  context: ToolContext,
  directory?: string,
): Promise<{ root: string; project: string | null } | null> {
  const starts = directory ? [directory] : [context.directory, process.cwd(), context.worktree]
  for (const s of starts) {
    if (!s) continue
    // Canonicalise so relative/trailing-slash/symlinked spellings of the same
    // directory all resolve to the same frame.
    let dir = path.resolve(s)
    try {
      dir = realpathSync(dir)
    } catch {
      // A non-existent path keeps its lexical form; git below fails closed.
    }

    // The documented layout: `<main>/apg/.worktrees/<project>` — the worktree
    // root is the segment right after `.worktrees`.
    const segs = dir.split(path.sep)
    for (let i = 0; i + 2 < segs.length; i++) {
      if (segs[i] === "apg" && segs[i + 1] === ".worktrees" && segs[i + 2]) {
        return { root: segs.slice(0, i).join(path.sep) || path.sep, project: segs[i + 2] }
      }
    }

    // Otherwise fall back to git: a linked worktree's git dir is
    // `<main>/.git/worktrees/<project>`; the main checkout's is `<main>/.git`.
    const out = await Bun.$`git rev-parse --git-dir`.cwd(dir).quiet().nothrow()
    if (out.exitCode !== 0) continue
    const gitDirRaw = out.stdout.toString().trim()
    if (!gitDirRaw) continue
    const gitDir = path.resolve(dir, gitDirRaw)
    const linked = /^(.*)[/\\]worktrees[/\\]([^/\\]+)$/.exec(gitDir)
    if (linked) return { root: path.dirname(linked[1]), project: linked[2] }
    return { root: path.dirname(gitDir), project: null }
  }
  return null
}

/**
 * Canonicalises a filesystem path into the SAME symlink-resolved frame
 * `mainCheckoutRoot` resolves its root in: the absolute path with every symlink
 * on its EXISTING prefix followed to its real target. A path that does not
 * exist yet cannot be `realpath`'d, so the deepest EXISTING ancestor is
 * resolved and the remaining (non-existent) segments are re-appended lexically
 * — a destination like `<worktree>/src/new.rs` stays comparable to the resolved
 * frame, while a symlink anywhere on the existing prefix (a `directory` spelled
 * through one, or a worktree-internal link pointing into the main checkout) is
 * followed to its real target and judged on it. This is what keeps the fs
 * tools' candidate and root comparisons in ONE frame; `fsScopeDecision` stays
 * pure and I/O-free.
 *
 * Real I/O (fs), so it is covered by the opt-in suite e2e, never the pure unit
 * tier.
 */
export function canonicalPath(p: string): string {
  let abs = path.resolve(p)
  const tail: string[] = []
  while (true) {
    try {
      const real = realpathSync(abs)
      return tail.length === 0 ? real : path.join(real, ...tail)
    } catch {
      const parent = path.dirname(abs)
      if (parent === abs) return path.join(abs, ...tail)
      tail.unshift(path.basename(abs))
      abs = parent
    }
  }
}

/**
 * The stored↔absolute identity boundary, side-effect-free (no fs, no
 * subprocess, no `Bun.$`): given the caller's project directory and a path,
 * it returns the OTHER spelling of the same file.
 *
 * - A stored repo-relative identity (the graph's File `fqn` / Struct/Function
 *   `path` — `/`-separated, no leading `/`) resolves to the absolute path
 *   under `projectDir`, so a consumer can open it.
 * - An input that is already absolute and under `projectDir` maps back to the
 *   stored repo-relative value, ready for a Cypher path column.
 *
 * The two directions round-trip. An absolute path outside `projectDir` (there
 * is no stored spelling to recover) and the empty string pass through
 * unchanged.
 */
export function resolveProjectPath(projectDir: string, value: string): string {
  if (!value) return value
  if (path.isAbsolute(value)) {
    const rel = path.relative(projectDir, value)
    if (rel === "" || rel.startsWith("..") || path.isAbsolute(rel)) return value
    return rel.split(path.sep).join("/")
  }
  return path.join(projectDir, value)
}

// ── Filesystem scope enforcement (apg_rm / apg_mv / apg_cp) ────────────────
//
// The three fs tools (`opencode-suite/tools/apg_rm.ts` / `apg_mv.ts` /
// `apg_cp.ts`) SELF-ENFORCE per-path scope so a code-writer implementer can
// implement plan `deletes`/`renames`/`moves` tasks only inside the surface it
// owns. The opencode tool-level grant is just the callability gate; the real
// boundary is decided here. The DECISION is PURE (`fsScopeDecision`: no fs, no
// process, no db) so the side-effect-free `bun test` unit tier covers it; the
// acting-agent permission read (`agentFsGlobs`) is real file I/O, covered by
// the opt-in suite e2e. This module stays PLUGIN-FREE — the acting agent name
// arrives as a plain string on the tool context, so nothing here imports
// `@opencode-ai/plugin` at runtime.
//
// MAIN-ANCHORED FRAME: the granted globs are written against the session
// workspace root (the MAIN checkout) — a code-writer's grants are
// `apg/.worktrees/*/<glob>` — so the decision resolves both the grants and the
// candidate against that one main root and structurally refuses any candidate
// that resolves into the main checkout (outside every project worktree). The
// main root is discovered by `mainCheckoutRoot` and handed in as an argument,
// and every candidate is canonicalised into that same frame by the
// `canonicalPath` I/O helper (which follows symlinks, resolving the deepest
// existing ancestor for a not-yet-existing destination) BEFORE the decision;
// `fsScopeDecision` itself stays pure.

/** The outcome of checking one candidate path against one acting agent's
 *  grants. `allowed` requires the resolved path to stay inside the frame root
 *  (`inBoundary`: no `..`/absolute escape), to land in a project worktree
 *  rather than the main checkout in main-anchored mode, and to match a granted
 *  glob (`globMatched`). `reason` is the human-readable refusal, or `null` when
 *  allowed. */
export interface FsScopeDecision {
  allowed: boolean
  /** `candidate`, resolved and expressed relative to the frame root (the main
   *  checkout root in main-anchored mode) in `/`-form — the spelling the
   *  granted globs match. */
  relativePath: string
  /** True when `relativePath` matched at least one granted glob. A main-checkout
   *  path can match a root glob yet still be refused by the worktree rule. */
  globMatched: boolean
  /** True when the resolved path stays inside the frame root (no `..`/absolute
   *  escape). */
  inBoundary: boolean
  /** Why the path was refused, or `null` when `allowed`. */
  reason: string | null
}

/**
 * Compiles one path glob to an anchored RegExp. A single star matches within a
 * path segment; a double star crosses path separators; a question mark matches
 * one non-separator character. A double star immediately followed by a
 * separator also matches the zero-directory case (so the Rust-file doublestar
 * pattern matches a bare basename). Every other metacharacter is escaped
 * literally. Globs match slash-separated relative paths.
 */
const globToRegExp = (glob: string): RegExp => {
  let re = ""
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i]
    if (c === "*") {
      if (glob[i + 1] === "*") {
        i++
        if (glob[i + 1] === "/") {
          i++
          re += "(?:.*/)?"
        } else {
          re += ".*"
        }
      } else {
        re += "[^/]*"
      }
    } else if (c === "?") {
      re += "[^/]"
    } else if ("\\^$.|+()[]{}".includes(c)) {
      re += "\\" + c
    } else {
      re += c
    }
  }
  return new RegExp("^" + re + "$")
}

/** True when a `/`-separated relative path matches a path glob. */
const globMatch = (glob: string, relativePath: string): boolean =>
  globToRegExp(glob).test(relativePath)

/**
 * The PURE filesystem scope decision. No fs, no process, no db, no git — safe
 * for the side-effect-free `bun test` unit tier, and the direct, plugin-free
 * target of the opt-in suite e2e (`opencode-suite/tests/boundary.e2e.test.ts`).
 *
 * - `mainRoot` — the frame root; `candidatePath` (absolute, or relative to it)
 *   is resolved against it. In main-anchored mode this is the MAIN checkout
 *   root (discovered by `mainCheckoutRoot`), the same root opencode resolves an
 *   agent's path-scoped grants against, so a worktree-rooted grant matches
 *   exactly the worktree path it names.
 * - `grantedGlobs` — the acting agent's granted path globs (its write scope,
 *   read from its permission block by `agentFsGlobs`); a literal argument here
 *   so the decision never depends on the plugin context.
 * - `project` — the acting worktree's project name `<p>` (or `null` when the
 *   caller is in the main checkout). Passing it puts the decision in
 *   MAIN-ANCHORED mode: the candidate must resolve under
 *   `<mainRoot>/apg/.worktrees/<p>/` (a `null` project accepts any project
 *   worktree), so a path that resolves into the main checkout is refused
 *   structurally — independent of the globs. Omitting it keeps the legacy
 *   single-root decision for a pure, non-worktree caller.
 *
 * Two INDEPENDENT tests, both required: the resolved path must stay inside
 * `mainRoot` (`inBoundary` — a `..`/absolute escape is refused even when it
 * matches a glob), and — in main-anchored mode — must land in a project
 * worktree, not the main checkout (refused with `path resolves into the main
 * checkout` even when a root glob matches); its main-root-relative `/`-form
 * must then match at least one granted glob (`globMatched`). This function is
 * PURE and resolves LEXICALLY only (no fs): callers canonicalise the frame root
 * and each candidate with the `canonicalPath` I/O helper first, so both sides
 * share ONE symlink-resolved frame — a `directory` spelled through a symlink and
 * a worktree-internal symlink pointing into the main checkout are judged on
 * their real target, never the caller's textual spelling.
 */
export function fsScopeDecision(
  mainRoot: string,
  candidatePath: string,
  grantedGlobs: readonly string[],
  project?: string | null,
): FsScopeDecision {
  const rootAbs = path.resolve(mainRoot)
  const candidateAbs = path.isAbsolute(candidatePath)
    ? path.resolve(candidatePath)
    : path.resolve(rootAbs, candidatePath)
  const rel = path.relative(rootAbs, candidateAbs)
  const inBoundary = rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel))
  const relativePath = rel.split(path.sep).join("/")

  // Main-anchored mode is active whenever the caller supplies `project`
  // (a worktree name, or `null` from the main checkout); `undefined` keeps the
  // legacy single-root decision. A write must land in a project worktree —
  // never the main checkout — so the structural worktree test runs before the
  // glob test and cannot be satisfied away by a root glob.
  const mainAnchored = project !== undefined
  const worktreePrefix = project ? `apg/.worktrees/${project}/` : null
  const inWorktree = !mainAnchored
    ? true
    : worktreePrefix !== null
      ? relativePath.startsWith(worktreePrefix)
      : /^apg\/\.worktrees\/[^/]+\//.test(relativePath)

  const globMatched = inBoundary && grantedGlobs.some((g) => globMatch(g, relativePath))
  const allowed = inBoundary && inWorktree && globMatched
  const reason = allowed
    ? null
    : !inBoundary
      ? "path escapes the project/worktree boundary"
      : !inWorktree
        ? "path resolves into the main checkout"
        : "path is not within the acting agent's granted globs"
  return { allowed, relativePath, globMatched, inBoundary, reason }
}

/**
 * Extracts the YAML frontmatter body (between the leading `---` fences) from an
 * agent config file, or `null` when the file is not a fenced-frontmatter doc.
 */
function frontmatter(text: string): string | null {
  const lines = text.split("\n")
  if (lines[0]?.trim() !== "---") return null
  const end = lines.indexOf("---", 1)
  if (end < 0) return null
  return lines.slice(1, end).join("\n")
}

/** Leading-space count of a line (indentation depth). */
function indentOf(line: string): number {
  return line.length - line.trimStart().length
}

/**
 * The `permission.edit` rules from an agent's frontmatter `permission` block —
 * the agent's write scope. Extracts the `edit:` mapping (a line whose only
 * content is `edit:` under `permission:`) and collects EVERY child entry
 * spelled `"<glob>": allow` or `"<glob>": deny` (quoted or bare key) IN FILE
 * ORDER, as ordered `{ glob, allow }` entries. Both kinds survive — allow
 * entries grant, deny entries revoke, and their order is the precedence a
 * last-matching-rule decision applies (a later entry overrides an earlier one,
 * so duplicates are preserved, not deduped). Returns `[]` (fail-closed) when
 * the body carries no `edit:` block.
 */
export function editAllowGlobs(frontmatterBody: string): { glob: string; allow: boolean }[] {
  const lines = frontmatterBody.split("\n")
  const start = lines.findIndex((l) => /^\s+edit:\s*$/.test(l))
  if (start < 0) return []
  const baseIndent = indentOf(lines[start])
  const rules: { glob: string; allow: boolean }[] = []
  for (let i = start + 1; i < lines.length; i++) {
    const line = lines[i]
    if (line.trim() === "") continue
    if (indentOf(line) <= baseIndent) break
    const m = /^\s+(?:"([^"]+)"|'([^']+)'|(\S+?)):\s*(allow|deny)\s*$/.exec(line)
    if (m) {
      const glob = m[1] ?? m[2] ?? m[3]
      rules.push({ glob, allow: m[4] === "allow" })
    }
  }
  return rules
}

/** Reads one agent config file and returns its `permission.edit` allow globs,
 *  or `null` when the file does not exist / cannot be read. */
function readAgentEditGlobs(file: string): string[] | null {
  if (!existsSync(file)) return null
  try {
    const fm = frontmatter(readFileSync(file, "utf8"))
    if (fm === null) return []
    // Interim adaptation (worktree-write-scope phase-04.task-4): `editAllowGlobs`
    // now returns the ordered allow+deny rule list. This reader's semantic
    // change is owned by task-4.12, so map it back to the legacy allow-only,
    // deduped glob list for now — `agentFsGlobs` and the fs tools are unchanged.
    const allowOnly: string[] = []
    for (const rule of editAllowGlobs(fm)) {
      if (rule.allow && !allowOnly.includes(rule.glob)) allowOnly.push(rule.glob)
    }
    return allowOnly
  } catch {
    return null
  }
}

/**
 * Resolves the ACTING agent (from the plain `context.agent` string the opencode
 * tool context carries) to its granted path globs. There is no permission field
 * on the tool context, so the grants are read from the agent's config file —
 * markdown with a fenced YAML frontmatter whose `permission.edit` block lists
 * the agent's path grants.
 *
 * An agent's config is authored on the MAIN checkout (agents are created and
 * committed there; there is no per-worktree `.opencode/agents` mirror), so the
 * main checkout root is the authoritative frame. A caller resolves it with
 * `mainCheckoutRoot(context, directory)` and passes `frame.root` as `mainRoot`:
 * the config is then read from `<mainRoot>/.opencode/agents/<agent>.md` and the
 * caller's `directory` is NOT consulted, so the resolved grants are identical
 * for every spelling of that directory (relative, trailing slash, symlink). The
 * grants are returned VERBATIM — already main-root-relative, the worktree-rooted
 * `apg/.worktrees/<project>/<glob>` forms `fsScopeDecision` matches against in
 * its main-anchored mode; the reader never rewrites them.
 *
 * With no `mainRoot` the legacy search is kept so an existing caller is
 * unaffected: the caller's own directories (`directory`, `context.directory`,
 * `context.worktree`), then the globally-installed suite
 * (`~/.opencode/agents/<agent>.md`, where `apg init` installs the distributed
 * agents). Returns `[]` when no agent name is present or no config resolves, so
 * a caller refuses every path rather than allowing one by default.
 *
 * Real file I/O, so it is covered by the opt-in suite e2e, never the pure unit
 * tier. It stays plugin-free: the name is a structural field, not a plugin type.
 */
export function agentFsGlobs(
  context: ToolContext,
  directory?: string,
  mainRoot?: string | null,
): string[] {
  const name = context.agent
  if (!name) return []
  // A resolved main root is the whole search frame: the agent file lives on main
  // and its grants are written against that root. Without one, keep the legacy
  // caller-directory search so an existing 2-argument caller still works.
  const roots = (mainRoot ? [mainRoot] : [directory, context.directory, context.worktree]).filter(
    (p): p is string => typeof p === "string" && p.length > 0,
  )
  for (const root of roots) {
    const globs = readAgentEditGlobs(path.join(root, ".opencode", "agents", `${name}.md`))
    if (globs !== null) return globs
  }
  const home = process.env.HOME || homedir()
  if (home) {
    const globs = readAgentEditGlobs(path.join(home, ".opencode", "agents", `${name}.md`))
    if (globs !== null) return globs
  }
  return []
}

/**
 * The rebase-column set for one row of `apg_find_symbol`'s result, whose
 * columns are `kind, n.fqn, n.path, n.start_line, n.end_line`.
 *
 * `apg_find_symbol` matches every node kind through one query, but the stored
 * repo-relative identity lands in a DIFFERENT cell depending on the kind: a
 * File row's `n.fqn` (column 1) IS its stored source path and File nodes carry
 * no `path` (column 2 is empty), while a Struct/Function row's `n.fqn` is a
 * symbol name and its `n.path` (column 2) is the stored source path. So a File
 * row rebases its fqn cell and every other kind rebases its path cell — never
 * both blindly, because a symbol fqn is not a path (`resolveProjectPath` would
 * join it under the project directory into a bogus path).
 *
 * Pure: no fs, no subprocess, no `Bun.$` — safe for the `bun test` unit tier.
 */
export function findSymbolRebaseColumns(row: string[]): number[] {
  return row[0] === "File" ? [1, 2] : [2]
}


/** Prefix of the error string runCypher returns when `apg query` exits non-zero;
 *  the exit code and the CLI's stderr follow it. Shared with `isQueryError` so
 *  the producer and the discriminant cannot silently drift apart. */
export const QUERY_FAILED_PREFIX = "apg query failed"

/** The error string runCypher returns when no `apg/.trans/db.lbug` is found on
 *  the walk-up. Shared with `isQueryError` so the producer and the discriminant
 *  cannot silently drift apart. */
export const NO_DB_ERROR =
  "Error: no apg/.trans/db.lbug found. Run `apg scan` in the project root first."

/**
 * Runs a Cypher query against the project's db and returns CSV text with a
 * header row on success. On failure it returns an error STRING — never a throw,
 * never data — that `isQueryError` recognizes: either `NO_DB_ERROR` (no db
 * found) or a string beginning with `QUERY_FAILED_PREFIX`, carrying `apg
 * query`'s exit code and stderr verbatim. Both failure signals are built from
 * the constants above, which `isQueryError` also tests against, so the failure
 * contract stays structural rather than duplicated string literals.
 *
 * DEFAULT-RAW seam (`requirements.requirement.absolute-paths-at-tool-boundary`):
 * with no `opts`, the stored form is returned verbatim, so raw `apg query` is
 * untouched. A curated tool opts in by naming the CSV columns that carry a
 * stored repo-relative identity; those cells are resolved to absolute paths
 * under the caller's project directory before the CSV is re-serialized. A
 * tool whose identity cell depends on the row's kind (e.g. `apg_find_symbol`,
 * where a File's identity is its fqn cell and a symbol's is its path cell)
 * passes a predicate `(row) => number[]` instead of a fixed column list. The
 * header row is never rebased.
 */
export async function runCypher(
  context: ToolContext,
  cypher: string,
  directory?: string,
  opts?: { rebaseColumns?: number[] | ((row: string[]) => number[]) },
): Promise<string> {
  const root = findApgRoot(context, directory)
  if (!root) {
    return NO_DB_ERROR
  }
  const result = await Bun.$`${apgBinary()} query ${cypher}`.cwd(root).quiet().nothrow()
  if (result.exitCode !== 0) {
    return `${QUERY_FAILED_PREFIX} (exit ${result.exitCode}):\n${result.stderr.toString().trim()}`
  }
  const out = result.stdout.toString().trim()
  const colsOpt = opts?.rebaseColumns
  if (!colsOpt) return out
  if (Array.isArray(colsOpt) && colsOpt.length === 0) return out
  const rebased = csvToRows(out).map((row, i) => {
    if (i === 0) return row
    const cols = typeof colsOpt === "function" ? colsOpt(row) : colsOpt
    return row.map((cell, j) => (cols.includes(j) ? resolveProjectPath(root, cell) : cell))
  })
  return rebased
    .map((row) =>
      row
        .map((cell) => (/[",\n\r]/.test(cell) ? `"${cell.replace(/"/g, '""')}"` : cell))
        .join(","),
    )
    .join("\n")
}

/** Single-quotes a value for a Cypher string literal, escaping \ ' " and newlines. */
export function lit(value: string): string {
  return (
    "'" +
    value
      .replace(/\\/g, "\\\\")
      .replace(/'/g, "\\'")
      .replace(/"/g, '\\"')
      .replace(/\n/g, "\\n")
      .replace(/\r/g, "\\r") +
    "'"
  )
}

/**
 * Returns the `alias.code_type = '...'` condition for a codeType arg, or ""
 * for "all"/empty (include everything, like the raw graph). Callers assemble
 * conditions into a WHERE clause.
 */
export function codeTypeCondition(alias: string, codeType?: string): string {
  const ct = codeType || "all"
  if (ct === "all") return ""
  return `${alias}.code_type = ${lit(ct)}`
}

/** Appends `note` when the query returned no data rows (just the header). */
export function noteIfEmpty(out: string, note: string): string {
  const lines = out.split("\n").filter((l) => l.length > 0)
  if (lines.length <= 1) return `${out}\n${note}`
  return out
}

/**
 * Runs an `apg` CLI subcommand (`apg node …` / `apg edge …` / `apg plan …` /
 * `apg review …` / `apg project …`) from the project root, returning its
 * stdout (or an error string prefixed with the subcommand). Authoring tools
 * are thin wrappers over this.
 */
export async function runCli(context: ToolContext, args: string[], directory?: string): Promise<string> {
  const root = findApgRoot(context, directory)
  if (!root) {
    return NO_DB_ERROR
  }
  const result = await Bun.$`${apgBinary()} ${args}`.cwd(root).quiet().nothrow()
  if (result.exitCode !== 0) {
    const cmd = [apgBinary(), ...args].join(" ")
    return `${cmd} failed (exit ${result.exitCode}):\n${result.stderr.toString().trim()}`
  }
  return result.stdout.toString().trim()
}

/** Extracts the project name from a project-scoped plan/review FQN
 * (`<project>/plan.phase-01`, `<project>/feedback-1`). Callers must already
 * know the FQN is plan-family (plan/phase/task/feedback nodes, never code or
 * durable layer nodes) — durable layer FQNs (`<layer>.<type>.<name>`) carry
 * no project prefix. */
export function projectOf(fqn: string): string | null {
  const m = /^([^/]+)\//.exec(fqn)
  return m ? m[1] : null
}

/** Parses `apg query`'s CSV output (header + rows, quoted fields) into rows.
 *  Guards the result first: a `runCypher` error string (query failure or
 *  missing DB) throws verbatim from `expectQueryOk` rather than being
 *  misparsed as data. Calling the parse boundary is therefore sufficient —
 *  callers cannot forget the guard. */
export function csvToRows(out: string): string[][] {
  const rows: string[][] = []
  for (const line of expectQueryOk(out).split("\n")) {
    if (line.length === 0) continue
    rows.push(parseCsvLine(line))
  }
  return rows
}

/**
 * True when a runCypher result is an error string (query failure or missing
 * DB), not CSV data — checked before it is ever fed to csvToRows. Tests the
 * exact signals runCypher builds (`QUERY_FAILED_PREFIX` / `NO_DB_ERROR`), so
 * the two cannot drift apart.
 */
export function isQueryError(out: string): boolean {
  return out.startsWith(QUERY_FAILED_PREFIX) || out.startsWith(NO_DB_ERROR)
}

/**
 * Guards a runCypher result: throws with the raw message when the query
 * failed, so the real `apg query failed` error surfaces instead of being
 * misparsed as CSV (which previously crashed with "undefined is not an object
 * (evaluating 'body.replace')"). Call before csvToRows(...).
 */
export function expectQueryOk(out: string): string {
  if (isQueryError(out)) throw new Error(out)
  return out
}

function parseCsvLine(line: string): string[] {
  const fields: string[] = []
  let cur = ""
  let inQuotes = false
  for (let i = 0; i < line.length; i++) {
    const c = line[i]
    if (inQuotes) {
      if (c === '"') {
        if (line[i + 1] === '"') {
          cur += '"'
          i++
        } else {
          inQuotes = false
        }
      } else {
        cur += c
      }
    } else if (c === '"') {
      inQuotes = true
    } else if (c === ",") {
      fields.push(cur)
      cur = ""
    } else {
      cur += c
    }
  }
  fields.push(cur)
  return fields
}

/**
 * True when `fqn` exists as a code node (Function/Struct/File) in the graph.
 * Label-alternation/OR in WHERE is unsupported, so each label is checked
 * separately. Used by the lint tools for planned-node realization and drift checks.
 */
export async function resolvesInCode(context: ToolContext, fqn: string): Promise<boolean> {
  for (const label of ["Function", "Struct", "File"]) {
    const out = await runCypher(context, `MATCH (n:${label} {fqn: ${lit(fqn)}}) RETURN n.fqn`)
    if (out.split("\n").filter((l) => l.length > 0).length > 1) return true
  }
  return false
}

/**
 * True when `fqn` is a `planned` Implementation node (a plan-writer-authored
 * placeholder awaiting realization) or a proposed Solution node — the pending
 * anchors of the model. Planned nodes are detected by `status: planned`;
 * pending solution anchors by a tier-3 Solution label.
 */
export async function isPendingAnchor(context: ToolContext, fqn: string): Promise<boolean> {
  for (const label of ["Struct", "Function", "File", "Module"]) {
    const out = await runCypher(
      context,
      `MATCH (n:${label} {fqn: ${lit(fqn)}}) WHERE n.status = 'planned' RETURN n.fqn`,
    )
    if (out.split("\n").filter((l) => l.length > 0).length > 1) return true
  }
  for (const label of ["System", "Container", "Component"]) {
    const out = await runCypher(context, `MATCH (n:${label} {fqn: ${lit(fqn)}}) RETURN n.fqn`)
    if (out.split("\n").filter((l) => l.length > 0).length > 1) return true
  }
  return false
}

// ── Project requirement scoping (apg_plan_phases) ──────────────────────────
//
// `apg_plan_phases` must report only the requirements the CURRENT
// project/branch is responsible for. A project's plan is transient and dies
// with its branch, so requirements delivered by earlier projects keep no
// surviving `Satisfies` edge and used to read "unsatisfied" forever. The
// scoping DECISION is a PURE function here (no fs, no subprocess, no db, no
// git) so the side-effect-free `bun test` unit tier can cover it; the git
// branch delta that feeds it is a thin subprocess WRAPPER (real I/O, hence
// e2e — see `opencode-suite/tests/boundary.e2e.test.ts`).

/** The layer directory holding one `<name>.json` node file per requirement —
 *  the file basename IS the requirement name. */
export const REQUIREMENT_LAYER_DIR = "apg/layers/requirements/requirement"

/** The fixed FQN stem every requirement node carries
 *  (`requirements.requirement.<name>`). */
export const REQUIREMENT_FQN_PREFIX = "requirements.requirement."

/** The project-scoped view of the requirement layer. */
export interface RequirementScope {
  /** In-scope requirement FQNs — branch-added ∪ this project's Satisfied — in
   *  the graph's requirement order (see `scopeProjectRequirements`). */
  inScope: string[]
  /** In-scope requirement FQNs no phase of THIS project Satisfies. */
  unsatisfied: string[]
  /** In-scope requirements Satisfied by more than one phase of THIS project,
   *  with those (this project's) phase FQNs. */
  overSatisfied: Array<{ requirement: string; phases: string[] }>
}

/**
 * The PURE project-requirement scoping decision. No fs, no subprocess, no
 * `Bun.$`, no db, no git — safe for the `bun test` unit tier.
 *
 * - `allRequirements` — the full requirement FQN set (the graph's
 *   `Requirement` nodes). It orders `inScope` (the emitted findings follow the
 *   graph's requirement order), and any in-scope requirement it does not list
 *   is appended, so the union is never truncated.
 * - `branchAdded` — the branch-added requirement NAMES (the
 *   `apg/layers/requirements/requirement/<name>.json` basenames); each maps to
 *   `requirements.requirement.<name>`.
 * - `satisfies` — THIS project's `Satisfies` relation: requirement FQN -> the
 *   this-project phase FQNs that Satisfy it. The caller filters to this
 *   project's phases (another project's `Satisfies` edge never enters the map),
 *   so more than one value means more than one phase OF THIS PROJECT.
 *
 * In scope = branch-added requirements ∪ requirements this project's phases
 * Satisfy. A requirement is `unsatisfied` when it is in scope but no phase of
 * this project Satisfies it (equivalently, a branch-added requirement the
 * project's plan does not satisfy). Requirements delivered by earlier projects
 * — whose transient `Satisfies` edges did not survive their branch — are
 * outside the scope and are not reported.
 */
export function scopeProjectRequirements(
  allRequirements: Iterable<string>,
  branchAdded: Iterable<string>,
  satisfies: ReadonlyMap<string, readonly string[]>,
): RequirementScope {
  const added = new Set<string>()
  for (const name of branchAdded) added.add(`${REQUIREMENT_FQN_PREFIX}${name}`)
  const satisfied = new Set(satisfies.keys())
  const scope = new Set<string>([...added, ...satisfied])

  const inScope: string[] = []
  const seen = new Set<string>()
  for (const fqn of allRequirements) {
    if (scope.has(fqn) && !seen.has(fqn)) {
      seen.add(fqn)
      inScope.push(fqn)
    }
  }
  for (const fqn of scope) {
    if (!seen.has(fqn)) {
      seen.add(fqn)
      inScope.push(fqn)
    }
  }

  return {
    inScope,
    unsatisfied: inScope.filter((fqn) => !satisfied.has(fqn)),
    overSatisfied: inScope
      .map((fqn) => ({ requirement: fqn, phases: [...(satisfies.get(fqn) ?? [])] }))
      .filter((entry) => entry.phases.length > 1),
  }
}

/**
 * The repo's default branch ref at `projectRoot`, or `null` when it cannot be
 * resolved. The NAME mirrors the binary's
 * `rust.apg.git.identity.repo_identity` default-branch resolution, origin-first:
 *
 * 1. `origin/HEAD`'s symbolic target when the remote-tracking symref exists
 *    (`origin/main` -> `main`) — the binary's `origin_default_branch`;
 * 2. else the MAIN checkout's symbolic HEAD — the primary `git worktree list`
 *    entry (the repo's original working tree), whose checked-out branch is the
 *    default — the binary's `main_checkout_head`;
 * 3. else `null` (the empty-delta fallback).
 *
 * The NAME is then peeled local-branch first (`refs/heads/<name>`), then
 * remote-tracking (`refs/remotes/origin/<name>`) — the same preference the
 * binary's `solution_nodes_added_on_branch` uses when it peels the default
 * branch.
 */
async function defaultBranchRef(projectRoot: string): Promise<string | null> {
  let name: string | null = null
  const head = await Bun.$`git symbolic-ref --short refs/remotes/origin/HEAD`
    .cwd(projectRoot)
    .quiet()
    .nothrow()
  const headRef = head.stdout.toString().trim()
  if (head.exitCode === 0 && headRef) name = headRef.replace(/^origin\//, "")

  // No `origin/HEAD`: the default is the MAIN checkout's checked-out branch.
  // `git worktree list --porcelain` lists the main worktree first; its symbolic
  // HEAD is the default. A detached/unborn main checkout yields nothing, so the
  // name stays unresolved and the caller sees an empty delta.
  if (!name) {
    const worktrees = await Bun.$`git worktree list --porcelain`
      .cwd(projectRoot)
      .quiet()
      .nothrow()
    if (worktrees.exitCode === 0) {
      const main = worktrees.stdout
        .toString()
        .split("\n")
        .find((line) => line.startsWith("worktree "))
        ?.slice("worktree ".length)
        .trim()
      if (main) {
        const mainHead = await Bun.$`git -C ${main} symbolic-ref --short HEAD`
          .cwd(projectRoot)
          .quiet()
          .nothrow()
        const mainRef = mainHead.stdout.toString().trim()
        if (mainHead.exitCode === 0 && mainRef) name = mainRef
      }
    }
  }

  if (!name) return null

  const local = await Bun.$`git rev-parse --verify --quiet refs/heads/${name}`
    .cwd(projectRoot)
    .quiet()
    .nothrow()
  if (local.exitCode === 0) return name
  const remote = await Bun.$`git rev-parse --verify --quiet refs/remotes/origin/${name}`
    .cwd(projectRoot)
    .quiet()
    .nothrow()
  if (remote.exitCode === 0) return `origin/${name}`
  return null
}

/**
 * The git branch delta for the REQUIREMENT layer: the requirement names whose
 * `apg/layers/requirements/requirement/<name>.json` node file is present on
 * this branch but absent from the repo's default branch's tree. Mirrors the
 * binary's `solution_nodes_added_on_branch` (a layer node file present on the
 * branch but absent from the default tree), adapted to the requirement layer.
 *
 * Real I/O — `Bun.$` git subprocesses with cwd at the project root — so it is
 * covered by the opt-in suite e2e, never the side-effect-free unit tier. The
 * pure `scopeProjectRequirements` consumes its result (the returned names feed
 * its `branchAdded` argument). Returns an empty set when no project root is
 * found, when no default branch resolves (a detached/unborn main checkout with
 * no `origin/HEAD`), or when the resolved name has neither a local nor a
 * remote-tracking ref, so the caller falls back to the Satisfies-only scope.
 * Only the first of these mirrors the binary's `default_branch`-None empty-delta
 * fallback; the unresolvable-ref case is an error there, and the suite stays
 * deliberately conservative (empty, not a throw).
 */
export async function branchAddedRequirementNames(
  context: ToolContext,
  directory?: string,
): Promise<Set<string>> {
  const root = findApgRoot(context, directory)
  if (!root) return new Set()
  const branch = await defaultBranchRef(root)
  if (!branch) return new Set()

  // The names present on this branch's working tree ...
  const dir = path.join(root, REQUIREMENT_LAYER_DIR)
  const names = existsSync(dir)
    ? readdirSync(dir)
        .filter((f) => f.endsWith(".json"))
        .map((f) => f.slice(0, -".json".length))
    : []

  // ... minus the names already carried by the default branch's tree.
  const listed = await Bun.$`git ls-tree -r --name-only ${branch} -- ${REQUIREMENT_LAYER_DIR}`
    .cwd(root)
    .quiet()
    .nothrow()
  const present = new Set<string>()
  if (listed.exitCode === 0) {
    for (const line of listed.stdout.toString().split("\n")) {
      const rel = line.trim()
      if (rel.endsWith(".json")) present.add(path.basename(rel, ".json"))
    }
  }

  const added = new Set<string>()
  for (const name of names) if (!present.has(name)) added.add(name)
  return added
}
