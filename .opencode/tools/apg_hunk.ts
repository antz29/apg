import { tool } from "@opencode-ai/plugin"
import { runCypher, lit } from "../lib/apg.ts"

export default tool({
  description:
    "Find the structs and functions that overlap a line range in one file. Use to map a diff hunk or review comment (startLine..endLine, inclusive) to the units it touches. Works with file line numbers, not byte offsets.",
  args: {
    path: tool.schema.string().describe("Absolute path of the file, e.g. /abs/src/Graph.java (required)"),
    startLine: tool.schema.string().describe("First line of the hunk, inclusive (required)"),
    endLine: tool.schema.string().describe("Last line of the hunk, inclusive (required)"),
    kind: tool.schema
      .string()
      .optional()
      .describe('Restrict to one kind: "Struct" or "Function". Default: both.'),
    limit: tool.schema.string().optional().describe("Max results (default 100, max 500)"),
  },
  async execute(args, context) {
    const path = args.path
    if (!path) return "Error: path is required"
    const startLine = Number(args.startLine)
    const endLine = Number(args.endLine)
    if (!Number.isFinite(startLine) || !Number.isFinite(endLine)) {
      return "Error: startLine and endLine must be numbers"
    }
    if (startLine > endLine) return "Error: startLine must be <= endLine"
    const limit = args.limit ? Math.max(1, Math.min(500, Number(args.limit))) : 100

    let cypher =
      `MATCH (n) WHERE n.path = ${lit(path)} AND n.start_line <= ${endLine} AND n.end_line >= ${startLine}`
    if (args.kind) {
      cypher += ` AND labels(n) = ${lit(args.kind)}`
    } else {
      cypher += ` AND (labels(n) = 'Struct' OR labels(n) = 'Function')`
    }
    cypher += ` RETURN labels(n) as kind, n.fqn, n.start_line, n.end_line ORDER BY n.start_line LIMIT ${limit}`
    return runCypher(context, cypher)
  },
})
