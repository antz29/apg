import { tool } from "@opencode-ai/plugin"
import { runCypher, lit } from "../lib/apg.ts"

export default tool({
  description:
    "Show unresolved references for a symbol or a whole file: calls and type uses the scanner could not resolve to a project symbol. Pass fqn for one unit, or path for every unit in a file.",
  args: {
    fqn: tool.schema.string().optional().describe("Function or Struct FQN to inspect (mutually exclusive with path)"),
    path: tool.schema.string().optional().describe("Absolute file path to inspect all units in (mutually exclusive with fqn)"),
    limit: tool.schema.string().optional().describe("Max results (default 200, max 1000)"),
  },
  async execute(args, context) {
    const fqn = args.fqn
    const path = args.path
    if (!fqn && !path) return "Error: provide fqn or path"
    if (fqn && path) return "Error: provide only one of fqn or path"
    const limit = args.limit ? Math.max(1, Math.min(1000, Number(args.limit))) : 200

    let cypher: string
    if (fqn) {
      cypher =
        `MATCH (n {fqn: ${lit(fqn)}})-[r:UnresolvedCall|UnresolvedUse]->(u:UnresolvedTarget) ` +
        `RETURN n.fqn as source, labels(r) as edge, u.fqn, u.category ORDER BY u.fqn LIMIT ${limit}`
    } else {
      cypher =
        `MATCH (f:File {fqn: ${lit(path)}})-[:Contains]->(n)-[r:UnresolvedCall|UnresolvedUse]->(u:UnresolvedTarget) ` +
        `RETURN n.fqn as source, labels(r) as edge, u.fqn, u.category ORDER BY n.fqn, u.fqn LIMIT ${limit}`
    }
    return runCypher(context, cypher)
  },
})
