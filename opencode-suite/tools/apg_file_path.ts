import { tool } from "@opencode-ai/plugin"
import { runCypher, lit } from "../lib/apg.ts"

export default tool({
  description:
    "Resolve a file path to its File node: line count, code type, and the module that contains it. Use to confirm a path is scanned or to map a file to its package/module.",
  args: {
    path: tool.schema.string().describe("Absolute path of the file, e.g. /abs/src/Graph.java (required)"),
  },
  async execute(args, context) {
    const path = args.path
    if (!path) return "Error: path is required"

    // Files normally hang under a Module; default-package Java files don't, so
    // fall back to the bare file row.
    const joined =
      `MATCH (m:Module)-[:Contains]->(f:File {fqn: ${lit(path)}}) ` +
      `RETURN m.fqn as module, f.fqn, f.start_line, f.end_line, f.code_type`
    const res = await runCypher(context, joined)
    if (res.includes("\n")) return res

    const bare =
      `MATCH (f:File {fqn: ${lit(path)}}) ` +
      `RETURN '' as module, f.fqn, f.start_line, f.end_line, f.code_type`
    return runCypher(context, bare)
  },
})
