import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, codeTypeCondition, noteIfEmpty } from "../lib/apg.ts"

export default tool({
  description:
    "Find every function the given function calls (outgoing Calls edges). Use to trace a function's dependencies or expand its call tree.",
  args: {
    fqn: tool.schema.string().describe("Source function FQN (required)"),
    codeType: tool.schema
      .string()
      .optional()
      .describe('Code type filter for callees: "src", "test", "generated", "external". Defaults to "all".'),
    limit: tool.schema.string().optional().describe("Max results (default 500, max 1000)"),
  },
  async execute(args, context) {
    const fqn = args.fqn
    if (!fqn) return "Error: fqn is required"
    const limit = args.limit ? Math.max(1, Math.min(1000, Number(args.limit))) : 500

    let cypher = `MATCH (f:Function {fqn: ${lit(fqn)}})-[:Calls]->(c:Function)`
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
