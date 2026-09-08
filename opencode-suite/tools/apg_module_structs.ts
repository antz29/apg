import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, codeTypeCondition } from "../lib/apg.ts"

export default tool({
  description:
    "List every struct/class/interface/enum declared anywhere under a module (two hops through its files). Use for 'all types in a package' style questions.",
  args: {
    fqn: tool.schema.string().describe("Module FQN, e.g. org.jgrapht.alg (required)"),
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

    let cypher =
      `MATCH (m:Module {fqn: ${lit(fqn)}})-[:Contains]->(:File)-[:Contains]->(s:Struct)`
    const ctCond = codeTypeCondition("s", args.codeType)
    if (ctCond) cypher += ` WHERE ${ctCond}`
    cypher += ` RETURN s.fqn, s.path, s.start_line, s.end_line, s.code_type ORDER BY s.fqn LIMIT ${limit}`
    return runCypher(context, cypher)
  },
})
