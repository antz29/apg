// Shared plumbing for the apg tool suite (installed by `apg init` into
// .opencode/lib/apg.ts). Lives outside .opencode/tools/ so opencode does not
// auto-discover it as a tool.
//
// Each suite tool shells out to `apg query` with a curated, fixed Cypher
// template. This module owns the project-root discovery, the subprocess call,
// and Cypher string-literal escaping (so structured args can never break out
// of or inject into a query).

import { existsSync } from "node:fs"
import path from "node:path"

export interface ToolContext {
  directory: string
  worktree: string
}

/** The apg binary to spawn: APG_BINARY env override, default "apg" on PATH. */
export function apgBinary(): string {
  return process.env.APG_BINARY || "apg"
}

/** Walks up from the session dirs looking for the project's `apg/.trans/db.lbug`.
 *  In a project worktree this resolves to the worktree's OWN `apg/` (and its
 *  branch DB) — never the main checkout's — so the tools work unchanged with
 *  cwd inside the worktree. */
export function findApgRoot(context: ToolContext): string | null {
  const starts = [context.directory, process.cwd(), context.worktree]
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
 * Runs a Cypher query against the project's db and returns CSV text with a
 * header row (or an error message prefixed with "apg query failed").
 */
export async function runCypher(context: ToolContext, cypher: string): Promise<string> {
  const root = findApgRoot(context)
  if (!root) {
    return "Error: no apg/.trans/db.lbug found. Run `apg scan` in the project root first."
  }
  const result = await Bun.$`${apgBinary()} query ${cypher}`.cwd(root).quiet().nothrow()
  if (result.exitCode !== 0) {
    return `apg query failed (exit ${result.exitCode}):\n${result.stderr.toString().trim()}`
  }
  return result.stdout.toString().trim()
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
export async function runCli(context: ToolContext, args: string[]): Promise<string> {
  const root = findApgRoot(context)
  if (!root) {
    return "Error: no apg/.trans/db.lbug found. Run `apg scan` in the project root first."
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

/** Parses `apg query`'s CSV output (header + rows, quoted fields) into rows. */
export function csvToRows(out: string): string[][] {
  const rows: string[][] = []
  for (const line of out.split("\n")) {
    if (line.length === 0) continue
    rows.push(parseCsvLine(line))
  }
  return rows
}

/**
 * True when a runCypher result is an error string (query failure or missing
 * DB), not CSV data — checked before it is ever fed to csvToRows.
 */
export function isQueryError(out: string): boolean {
  return out.startsWith("apg query failed") || out.startsWith("Error:")
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
