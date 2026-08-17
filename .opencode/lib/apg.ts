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

/** Walks up from the session dirs looking for the project's `.apg/db.lbug`. */
export function findApgRoot(context: ToolContext): string | null {
  const starts = [context.directory, process.cwd(), context.worktree]
  for (const s of starts) {
    if (!s) continue
    let dir = s
    while (true) {
      if (existsSync(path.join(dir, ".apg", "db.lbug"))) return dir
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
    return "Error: no .apg/db.lbug found. Run `apg scan` in the project root first."
  }
  const result = await Bun.$`apg query ${cypher}`.cwd(root).quiet().nothrow()
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
