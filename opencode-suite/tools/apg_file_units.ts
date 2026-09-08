import { tool } from "@opencode-ai/plugin"
import { runCypher, lit } from "../lib/apg.ts"

export default tool({
  description:
    "List everything declared in a source file (structs and functions) with their line ranges. The file's fqn is its absolute path. Use as the file-scoped view for reviews.",
  args: {
    path: tool.schema.string().describe("Absolute path of the file, e.g. /abs/src/Graph.java (required)"),
    limit: tool.schema.string().optional().describe("Max results (default 500, max 1000)"),
  },
  async execute(args, context) {
    const path = args.path
    if (!path) return "Error: path is required"
    const limit = args.limit ? Math.max(1, Math.min(1000, Number(args.limit))) : 500

    const cypher =
      `MATCH (f:File {fqn: ${lit(path)}})-[:Contains]->(n) ` +
      `RETURN labels(n) as kind, n.fqn, n.start_line, n.end_line ORDER BY n.start_line LIMIT ${limit}`
    return runCypher(context, cypher)
  },
})
