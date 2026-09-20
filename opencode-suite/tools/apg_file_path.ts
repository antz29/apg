import { tool } from "@opencode-ai/plugin"
import path from "node:path"
import { runCypher, lit, findApgRoot, resolveProjectPath } from "../lib/apg.ts"

export default tool({
  description:
    "Resolve a file path to its File node: line count, code type, and the module that contains it. Use to confirm a path is scanned or to map a file to its package/module.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    path: tool.schema.string().describe("Absolute path of the file, e.g. /abs/src/Graph.java (required)"),
  },
  async execute(args, context) {
    const filePath = args.path
    if (!filePath) return "Error: path is required"

    // The graph stores repo-relative identities; convert the caller's absolute
    // path to the stored form before querying, and rebase the returned File fqn
    // back to an absolute path (column 1).
    const root = findApgRoot(context, args.directory)
    const stored = path.isAbsolute(filePath) && root ? resolveProjectPath(root, filePath) : filePath

    // Files normally hang under a Module; default-package Java files don't, so
    // fall back to the bare file row.
    const joined =
      `MATCH (m:Module)-[:Contains]->(f:File {fqn: ${lit(stored)}}) ` +
      `RETURN m.fqn as module, f.fqn, f.start_line, f.end_line, f.code_type`
    const res = await runCypher(context, joined, args.directory, { rebaseColumns: [1] })
    if (res.includes("\n")) return res

    const bare =
      `MATCH (f:File {fqn: ${lit(stored)}}) ` +
      `RETURN '' as module, f.fqn, f.start_line, f.end_line, f.code_type`
    return runCypher(context, bare, args.directory, { rebaseColumns: [1] })
  },
})
