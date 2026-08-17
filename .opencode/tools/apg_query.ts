import { existsSync } from "node:fs"
import path from "node:path"
import { tool } from "@opencode-ai/plugin"

function findApgRoot(context: { directory: string; worktree: string }): string | null {
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

export default tool({
  description:
    "Execute a read-only Cypher query on the project's LadybugDB program graph (.apg/db.lbug). CSV output, header row included. Graph schema: Module ->(Contains)-> File ->(Contains)-> Struct/Function; Struct ->(Contains)-> Struct/Function for nested types and methods; Calls/Uses/Unresolved* edges link functions and types. Struct/Function/File nodes carry start_line/end_line (1-based) plus path — use them to join against diffs/hunks. MATCH/RETURN only. No modifications.",
  args: {
    query: tool.schema.string().describe("Cypher query, e.g. MATCH (f:File {fqn:'/abs/Graph.java'})-[:Contains]->(n) RETURN labels(n), n.fqn, n.start_line, n.end_line"),
  },
  async execute(args, context) {
    const root = findApgRoot(context)
    if (!root) {
      return `Error: no .apg/db.lbug found. Run \`apg scan\` in the project root first.`
    }
    const result = await Bun.$`apg query ${args.query}`.cwd(root).quiet().nothrow()
    if (result.exitCode !== 0) {
      return `apg query failed (exit ${result.exitCode}):\n${result.stderr.toString().trim()}`
    }
    return result.stdout.toString().trim()
  },
})
