import { Plugin } from "@opencode/plugin"
import path from "node:path"
import { runCypher, lit, findApgRoot, resolveProjectPath, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_file_path",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_file_path",
        description:
          "Resolve a file path to its File node: line count, code type, and the module that contains it. Use to confirm a path is scanned or to map a file to its package/module.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            path: { type: "string", description: "Absolute path of the file, e.g. /abs/src/Graph.java (required)" },
          },
          required: ["path"],
          additionalProperties: false,
        },
        async execute(input, context) {
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          const filePath = input.path
          if (!filePath) return { content: "Error: path is required" }

          // The graph stores repo-relative identities; convert the caller's absolute
          // path to the stored form before querying, and rebase the returned File fqn
          // back to an absolute path (column 1).
          const root = findApgRoot(merged, input.directory)
          const stored = path.isAbsolute(filePath) && root ? resolveProjectPath(root, filePath) : filePath

          // Files normally hang under a Module; default-package Java files don't, so
          // fall back to the bare file row.
          const joined =
            `MATCH (m:Module)-[:Contains]->(f:File {fqn: ${lit(stored)}}) ` +
            `RETURN m.fqn as module, f.fqn, f.start_line, f.end_line, f.code_type`
          const res = await runCypher(merged, joined, input.directory, { rebaseColumns: [1] })
          if (res.includes("\n")) return { content: res }

          const bare =
            `MATCH (f:File {fqn: ${lit(stored)}}) ` +
            `RETURN '' as module, f.fqn, f.start_line, f.end_line, f.code_type`
          return { content: await runCypher(merged, bare, input.directory, { rebaseColumns: [1] }) }
        },
      })
    })
  },
})
