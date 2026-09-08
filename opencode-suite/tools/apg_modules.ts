import { tool } from "@opencode-ai/plugin"
import { runCypher, lit } from "../lib/apg.ts"

export default tool({
  description:
    "List the Module nodes in the project (packages for Java, modules for Go/C++, namespaces for C++). Optionally filter by FQN prefix.",
  args: {
    prefix: tool.schema.string().optional().describe("Only return modules whose FQN contains this string"),
    limit: tool.schema.string().optional().describe("Max results (default 200, max 1000)"),
  },
  async execute(args, context) {
    const limit = args.limit ? Math.max(1, Math.min(1000, Number(args.limit))) : 200
    let cypher = `MATCH (m:Module)`
    if (args.prefix) {
      cypher += ` WHERE m.fqn CONTAINS ${lit(args.prefix)}`
    }
    cypher += ` RETURN m.fqn ORDER BY m.fqn LIMIT ${limit}`
    return runCypher(context, cypher)
  },
})
