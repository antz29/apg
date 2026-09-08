import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, codeTypeCondition } from "../lib/apg.ts"

export default tool({
  description:
    "List the source files in a module (package/directory), with their line counts. Each file's fqn is its absolute path; use apg_file_units to see what a file contains.",
  args: {
    fqn: tool.schema.string().describe("Module FQN, e.g. org.jgrapht.alg or github.com/org/repo (required)"),
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

    let cypher = `MATCH (m:Module {fqn: ${lit(fqn)}})-[:Contains]->(f:File)`
    const ctCond = codeTypeCondition("f", args.codeType)
    if (ctCond) cypher += ` WHERE ${ctCond}`
    cypher += ` RETURN f.fqn, f.start_line, f.end_line, f.code_type ORDER BY f.fqn LIMIT ${limit}`
    return runCypher(context, cypher)
  },
})
