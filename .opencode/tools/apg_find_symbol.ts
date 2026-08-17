import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, codeTypeCondition } from "../lib/apg.ts"

export default tool({
  description:
    "Search the project's code graph for symbols whose FQN contains a string. Returns matching Modules, Files, Structs, and Functions with their locations. Use to look up an exact FQN when you only know part of a name.",
  args: {
    name: tool.schema.string().describe("Substring to match against node FQNs (required)"),
    kind: tool.schema
      .string()
      .optional()
      .describe("Restrict to a node kind: Module, File, Struct, Function, UnresolvedTarget"),
    codeType: tool.schema
      .string()
      .optional()
      .describe('Code type filter: "src", "test", "generated", "external". Defaults to "all".'),
    limit: tool.schema
      .string()
      .optional()
      .describe("Max results (default 50, max 500)"),
  },
  async execute(args, context) {
    const name = args.name
    if (!name) return "Error: name is required"
    const kind = args.kind || ""
    const limit = args.limit ? Math.max(1, Math.min(500, Number(args.limit))) : 50

    let cypher = `MATCH (n) WHERE n.fqn CONTAINS ${lit(name)}`
    const conds: string[] = []
    if (kind) {
      conds.push(`labels(n) = ${lit(kind)}`)
    }
    const ctCond = codeTypeCondition("n", args.codeType)
    if (ctCond) conds.push(ctCond)
    if (conds.length) {
      cypher += ` AND ${conds.join(" AND ")}`
    }
    cypher += ` RETURN labels(n) as kind, n.fqn, n.path, n.start_line, n.end_line ORDER BY n.fqn LIMIT ${limit}`
    return runCypher(context, cypher)
  },
})
