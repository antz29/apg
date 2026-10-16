import { Plugin } from "@opencode/plugin"
import path from "node:path"
import { runCypher, lit, findApgRoot, resolveProjectPath, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_file_units",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_file_units",
        options: { codemode: false },
        description:
          "List everything declared in a source file (structs and functions) with their line ranges. Use as the file-scoped view for reviews.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            path: { type: "string", description: "Absolute path of the file, e.g. /abs/src/Graph.java (required)" },
            limit: { type: "string", description: "Max results (default 500, max 1000)" },
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
          const limit = input.limit ? Math.max(1, Math.min(1000, Number(input.limit))) : 500

          // The graph stores repo-relative identities; convert the caller's absolute
          // path to the stored form before querying.
          const root = findApgRoot(merged, input.directory)
          const stored = path.isAbsolute(filePath) && root ? resolveProjectPath(root, filePath) : filePath

          const cypher =
            `MATCH (f:File {fqn: ${lit(stored)}})-[:Contains]->(n) ` +
            `RETURN labels(n) as kind, n.fqn, n.start_line, n.end_line ORDER BY n.start_line LIMIT ${limit}`
          return { content: await runCypher(merged, cypher, input.directory) }
        },
      })
    })
  },
})
