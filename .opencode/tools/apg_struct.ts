import { tool } from "@opencode-ai/plugin"
import { runCypher, lit } from "../lib/apg.ts"

export default tool({
  description:
    "Show a struct/class/interface/enum: its file location and line range, plus any nested structs it directly contains.",
  args: {
    fqn: tool.schema.string().describe("Struct FQN, e.g. org.jgrapht.graph.DefaultGraphType (required)"),
  },
  async execute(args, context) {
    const fqn = args.fqn
    if (!fqn) return "Error: fqn is required"

    const self =
      `MATCH (s:Struct {fqn: ${lit(fqn)}}) ` +
      `RETURN s.fqn, s.path, s.start_line, s.end_line, s.code_type`
    const nested =
      `MATCH (s:Struct {fqn: ${lit(fqn)}})-[:Contains]->(n:Struct) ` +
      `RETURN n.fqn, n.start_line, n.end_line ORDER BY n.start_line`

    const [selfOut, nestedOut] = await Promise.all([runCypher(context, self), runCypher(context, nested)])
    const nestedBody = nestedOut.split("\n").slice(1).join("\n")
    return nestedBody ? `${selfOut}\n-- nested structs --\n${nestedBody}` : selfOut
  },
})
