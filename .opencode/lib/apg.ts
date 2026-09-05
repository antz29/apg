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

/** Walks up from the session dirs looking for the project's `apg/.trans/db.lbug`. */
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

/**
 * Runs an `apg` CLI subcommand (`apg spec …` / `apg plan …` / `apg review …`)
 * from the project root, returning its stdout (or an error string prefixed
 * with the subcommand). Authoring tools are thin wrappers over this.
 */
export async function runCli(context: ToolContext, args: string[]): Promise<string> {
  const root = findApgRoot(context)
  if (!root) {
    return "Error: no apg/.trans/db.lbug found. Run `apg scan` in the project root first."
  }
  const result = await Bun.$`apg ${args}`.cwd(root).quiet().nothrow()
  if (result.exitCode !== 0) {
    const cmd = ["apg", ...args].join(" ")
    return `${cmd} failed (exit ${result.exitCode}):\n${result.stderr.toString().trim()}`
  }
  return result.stdout.toString().trim()
}

/** Extracts the project name from a `future/<project>/…` FQN, or null. */
export function projectOf(fqn: string): string | null {
  const m = /^future\/([^/]+)\//.exec(fqn)
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
 * separately. Used by the lint tools for satisfiable-future and drift checks.
 */
export async function resolvesInCode(context: ToolContext, fqn: string): Promise<boolean> {
  for (const label of ["Function", "Struct", "File"]) {
    const out = await runCypher(context, `MATCH (n:${label} {fqn: ${lit(fqn)}}) RETURN n.fqn`)
    if (out.split("\n").filter((l) => l.length > 0).length > 1) return true
  }
  return false
}
