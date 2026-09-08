import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, codeTypeCondition } from "../lib/apg.ts"

export default tool({
  description:
    "Follow Uses edges (type references) for a symbol. direction=out lists the types a function/struct uses; direction=in lists who uses a struct. Note: calls to a struct's methods appear as Calls, not Uses.",
  args: {
    fqn: tool.schema.string().describe("Symbol FQN (required)"),
    direction: tool.schema
      .string()
      .optional()
      .describe('"out" (default): types X uses. "in": units that use X.'),
    codeType: tool.schema
      .string()
      .optional()
      .describe('Code type filter on the OTHER end of the edge (the used type for "out", the user for "in"). Defaults to "all".'),
    limit: tool.schema.string().optional().describe("Max results (default 500, max 1000)"),
  },
  async execute(args, context) {
    const fqn = args.fqn
    if (!fqn) return "Error: fqn is required"
    const direction = args.direction || "out"
    const limit = args.limit ? Math.max(1, Math.min(1000, Number(args.limit))) : 500

    let cypher: string
    if (direction === "in") {
      cypher = `MATCH (n)-[:Uses]->(s:Struct) WHERE s.fqn = ${lit(fqn)}`
      const ctCond = codeTypeCondition("n", args.codeType)
      if (ctCond) cypher += ` AND ${ctCond}`
      cypher += ` RETURN labels(n) as kind, n.fqn as user, n.path, n.start_line, n.end_line ORDER BY n.fqn LIMIT ${limit}`
    } else {
      cypher = `MATCH (n)-[:Uses]->(s:Struct) WHERE n.fqn = ${lit(fqn)}`
      const ctCond = codeTypeCondition("s", args.codeType)
      if (ctCond) cypher += ` AND ${ctCond}`
      cypher += ` RETURN s.fqn as used_type, s.path, s.start_line, s.end_line, s.code_type ORDER BY s.fqn LIMIT ${limit}`
    }
    return runCypher(context, cypher)
  },
})
