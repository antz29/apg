import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, codeTypeCondition, noteIfEmpty } from "../lib/apg.ts"

export default tool({
  description:
    "List the methods and functions of a struct/class/interface/enum (its Struct -> Function containment), with file locations and line ranges.",
  args: {
    fqn: tool.schema.string().describe("Struct FQN, e.g. org.jgrapht.Graph (required)"),
    codeType: tool.schema
      .string()
      .optional()
      .describe('Code type filter: "src", "test", "generated", "external". Defaults to "all".'),
    limit: tool.schema.string().optional().describe("Max results (default 500, max 1000)"),
  },
  async execute(args, context) {
    const fqn = args.fqn
    if (!fqn) return "Error: fqn is required"
    const limit = args.limit ? Math.max(1, Math.min(1000, Number(args.limit))) : 500

    let cypher = `MATCH (s:Struct {fqn: ${lit(fqn)}})-[:Contains]->(f:Function)`
    const ctCond = codeTypeCondition("f", args.codeType)
    if (ctCond) cypher += ` WHERE ${ctCond}`
    cypher += ` RETURN f.fqn, f.path, f.start_line, f.end_line, f.code_type ORDER BY f.fqn LIMIT ${limit}`
    const out = await runCypher(context, cypher)
    return noteIfEmpty(
      out,
      "no results (FQN is exact — overloads carry parameter suffixes; use apg_find_symbol to locate one)",
    )
  },
})
