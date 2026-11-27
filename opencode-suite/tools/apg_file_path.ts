import { Plugin } from "@opencode/plugin"
import path from "node:path"
import { runCypher, lit, resolveProjectPath, resolveProjectWorktree, type ToolContext } from "../lib/apg.ts"

type ApgFilePathInput = { project: string; path: string }

export default Plugin.define({
  id: "apg_file_path",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_file_path",
        options: { codemode: false },
        description:
          "Resolve a file path to its File node: line count, code type, and the module that contains it. Use to confirm a path is scanned or to map a file to its package/module.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            path: { type: "string", description: "Absolute path of the file, e.g. /abs/src/Graph.java (required)" },
          },
          required: ["project", "path"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgFilePathInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const filePath = input.path
          if (!filePath) return { content: "Error: path is required" }

          // The graph stores repo-relative identities; convert the caller's absolute
          // path to the stored form before querying, and rebase the returned File fqn
          // back to an absolute path (column 1).
          const root = merged.worktree
          const stored = path.isAbsolute(filePath) && root ? resolveProjectPath(root, filePath) : filePath

          // Files normally hang under a Module; default-package Java files don't, so
          // fall back to the bare file row.
          const joined =
            `MATCH (m:Module)-[:Contains]->(f:File {fqn: ${lit(stored)}}) ` +
            `RETURN m.fqn as module, f.fqn, f.start_line, f.end_line, f.code_type`
          const res = await runCypher(merged, joined, { rebaseColumns: [1] })
          if (res.includes("\n")) return { content: res }

          const bare =
            `MATCH (f:File {fqn: ${lit(stored)}}) ` +
            `RETURN '' as module, f.fqn, f.start_line, f.end_line, f.code_type`
          return { content: await runCypher(merged, bare, { rebaseColumns: [1] }) }
        },
      })
    })
  },
})
