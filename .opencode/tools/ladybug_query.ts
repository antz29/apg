import { tool } from "@opencode-ai/plugin"

export default tool({
  description:
    "Execute a read-only Cypher query on the LadybugDB graph database (CSV output, header row included). Use for graph traversal: MATCH/RETURN only. No modifications.",
  args: {
    query: tool.schema.string().describe("Cypher query, e.g. MATCH (n:Module) RETURN n.fqn LIMIT 10"),
  },
  async execute(args, context) {
    const q = args.query.endsWith(";") ? args.query : args.query + ";"
    const result =
      await Bun.$`printf '%s' ${q} | lbug -r -m csv -s -b ${context.worktree}/db.lbug 2>/dev/null`.text()
    return result.trim()
  },
})
