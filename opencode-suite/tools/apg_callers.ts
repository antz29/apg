import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, codeTypeCondition, noteIfEmpty } from "../lib/apg.ts"

export default tool({
  description:
    "Find every function that calls the given function (incoming Calls edges). Include the caller's location so you can jump to each call site.",
  args: {
    fqn: tool.schema.string().describe("Target function FQN, e.g. org.jgrapht.Graph.addVertex (required)"),
    codeType: tool.schema
      .string()
      .optional()
      .describe('Code type filter for callers: "src", "test", "generated", "external". Defaults to "all".'),
    limit: tool.schema.string().optional().describe("Max results (default 500, max 1000)"),
  },
  async execute(args, context) {
    const fqn = args.fqn
    if (!fqn) return "Error: fqn is required"
    const limit = args.limit ? Math.max(1, Math.min(1000, Number(args.limit))) : 500

    let cypher = `MATCH (c:Function)-[:Calls]->(t:Function {fqn: ${lit(fqn)}})`
    const ctCond = codeTypeCondition("c", args.codeType)
    if (ctCond) cypher += ` WHERE ${ctCond}`
    cypher += ` RETURN c.fqn, c.path, c.start_line, c.end_line, c.code_type ORDER BY c.fqn LIMIT ${limit}`
    const out = await runCypher(context, cypher)
    return noteIfEmpty(
      out,
      "no results (FQN is exact — overloads carry parameter suffixes; use apg_find_symbol to locate one)",
    )
  },
})
