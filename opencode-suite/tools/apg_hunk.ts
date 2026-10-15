import { Plugin } from "@opencode/plugin"
import path from "node:path"
import { runCypher, lit, findApgRoot, resolveProjectPath, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_hunk",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_hunk",
        description:
          "Find the structs and functions that overlap a line range in one file. Use to map a diff hunk or review comment (startLine..endLine, inclusive) to the units it touches. Works with file line numbers, not byte offsets.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            path: { type: "string", description: "Absolute path of the file, e.g. /abs/src/Graph.java (required)" },
            startLine: { type: "string", description: "First line of the hunk, inclusive (required)" },
            endLine: { type: "string", description: "Last line of the hunk, inclusive (required)" },
            kind: {
              type: "string",
              description: 'Restrict to one kind: "Struct" or "Function". Default: both.',
            },
            limit: { type: "string", description: "Max results (default 100, max 500)" },
          },
          required: ["path", "startLine", "endLine"],
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
          const startLine = Number(input.startLine)
          const endLine = Number(input.endLine)
          if (!Number.isFinite(startLine) || !Number.isFinite(endLine)) {
            return { content: "Error: startLine and endLine must be numbers" }
          }
          if (startLine > endLine) return { content: "Error: startLine must be <= endLine" }
          const limit = input.limit ? Math.max(1, Math.min(500, Number(input.limit))) : 100

          // The graph's `path` column is the stored repo-relative identity; convert
          // the caller's absolute path before matching.
          const root = findApgRoot(merged, input.directory)
          const stored = path.isAbsolute(filePath) && root ? resolveProjectPath(root, filePath) : filePath

          let cypher =
            `MATCH (n) WHERE n.path = ${lit(stored)} AND n.start_line <= ${endLine} AND n.end_line >= ${startLine}`
          if (input.kind) {
            cypher += ` AND labels(n) = ${lit(input.kind)}`
          } else {
            cypher += ` AND (labels(n) = 'Struct' OR labels(n) = 'Function')`
          }
          cypher += ` RETURN labels(n) as kind, n.fqn, n.start_line, n.end_line ORDER BY n.start_line LIMIT ${limit}`
          return { content: await runCypher(merged, cypher, input.directory) }
        },
      })
    })
  },
})
