import { existsSync } from "node:fs"
import path from "node:path"
import { tool } from "@opencode-ai/plugin"

function resolveProjectRoot(context: { directory: string; worktree: string }): string | null {
  const candidates = [
    context.directory,
    process.cwd(),
    context.worktree,
    path.resolve(import.meta.dir, "..", ".."),
  ]
  for (const c of candidates) {
    if (!c) continue
    if (existsSync(path.join(c, "db.lbug"))) {
      return c
    }
  }
  return null
}

export default tool({
  description:
    "Execute a read-only Cypher query on the LadybugDB graph database (CSV output, header row included). Use for graph traversal: MATCH/RETURN only. No modifications.",
  args: {
    query: tool.schema.string().describe("Cypher query, e.g. MATCH (n:Module) RETURN n.fqn LIMIT 10"),
  },
  async execute(args, context) {
    const q = args.query.endsWith(";") ? args.query : args.query + ";"
    const root = resolveProjectRoot(context)
    if (!root) {
      return `Error: db.lbug not found. Tried:\n  ${[
        context.directory,
        process.cwd(),
        context.worktree,
        path.resolve(import.meta.dir, "..", ".."),
      ]
        .filter(Boolean)
        .join("\n  ")}`
    }
    const db = path.join(root, "db.lbug")
    const result = await Bun.$`printf '%s' ${q} | lbug -r -m csv -s -b ${db}`.nothrow()
    if (result.exitCode !== 0) {
      return `lbug failed (exit ${result.exitCode}) on db ${db}:\n${result.stderr.toString().trim()}`
    }
    return result.stdout.toString().trim()
  },
})
