import { tool } from "@opencode-ai/plugin"
import { runCypher } from "../lib/apg.ts"

export default tool({
  description:
    "Execute a read-only Cypher query on the project's LadybugDB program graph (apg/.trans/db.lbug). CSV output, header row included. Graph schema: Module ->(Contains)-> File ->(Contains)-> Struct/Function; Struct ->(Contains)-> Struct/Function for nested types and methods; Calls/Uses/Unresolved* edges link functions and types. Struct/Function/File nodes carry start_line/end_line (1-based) plus path — use them to join against diffs/hunks. MATCH/RETURN only. No modifications.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    query: tool.schema.string().describe("Cypher query, e.g. MATCH (f:File {fqn:'/abs/Graph.java'})-[:Contains]->(n) RETURN labels(n), n.fqn, n.start_line, n.end_line"),
  },
  async execute(args, context) {
    return runCypher(context, args.query, args.directory)
  },
})
