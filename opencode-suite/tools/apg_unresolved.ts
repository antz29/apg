import { Plugin } from "@opencode/plugin"
import path from "node:path"
import { runCypher, lit, resolveProjectPath, resolveProjectWorktree, type ToolContext } from "../lib/apg.ts"

type ApgUnresolvedInput = { project: string; fqn?: string; path?: string; limit?: string }

export default Plugin.define({
  id: "apg_unresolved",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_unresolved",
        options: { codemode: false },
        description:
          "Show unresolved references for a symbol or a whole file: calls and type uses the scanner could not resolve to a project symbol. Pass fqn for one unit, or path for every unit in a file.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            fqn: { type: "string", description: "Function or Struct FQN to inspect (mutually exclusive with path)" },
            path: { type: "string", description: "Absolute file path to inspect all units in (mutually exclusive with fqn)" },
            limit: { type: "string", description: "Max results (default 200, max 1000)" },
          },
          required: ["project"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgUnresolvedInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const fqn = input.fqn
          const filePath = input.path
          if (!fqn && !filePath) return { content: "Error: provide fqn or path" }
          if (fqn && filePath) return { content: "Error: provide only one of fqn or path" }
          const limit = input.limit ? Math.max(1, Math.min(1000, Number(input.limit))) : 200

          let cypher: string
          if (fqn) {
            cypher =
              `MATCH (n {fqn: ${lit(fqn)}})-[r:UnresolvedCall|UnresolvedUse]->(u:UnresolvedTarget) ` +
              `RETURN n.fqn as source, labels(r) as edge, u.fqn, u.category ORDER BY u.fqn LIMIT ${limit}`
          } else {
            // The File fqn is the stored repo-relative identity; convert the caller's
            // absolute path before matching.
            const root = merged.worktree
            const stored = filePath && path.isAbsolute(filePath) && root ? resolveProjectPath(root, filePath) : filePath
            cypher =
              `MATCH (f:File {fqn: ${lit(stored ?? "")}})-[:Contains]->(n)-[r:UnresolvedCall|UnresolvedUse]->(u:UnresolvedTarget) ` +
              `RETURN n.fqn as source, labels(r) as edge, u.fqn, u.category ORDER BY n.fqn, u.fqn LIMIT ${limit}`
          }
          return { content: await runCypher(merged, cypher) }
        },
      })
    })
  },
})
