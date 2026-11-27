import { Plugin } from "@opencode/plugin"
import { runCypher, lit, resolveProjectWorktree, type ToolContext } from "../lib/apg.ts"

type ApgModulesInput = { project: string; prefix?: string; limit?: string }

export default Plugin.define({
  id: "apg_modules",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_modules",
        options: { codemode: false },
        description:
          "List the Module nodes in the project (packages for Java, modules for Go/C++, namespaces for C++). Optionally filter by FQN prefix.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            prefix: { type: "string", description: "Only return modules whose FQN contains this string" },
            limit: { type: "string", description: "Max results (default 200, max 1000)" },
          },
          required: ["project"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgModulesInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const limit = input.limit ? Math.max(1, Math.min(1000, Number(input.limit))) : 200
          let cypher = `MATCH (m:Module)`
          if (input.prefix) {
            cypher += ` WHERE m.fqn CONTAINS ${lit(input.prefix)}`
          }
          cypher += ` RETURN m.fqn ORDER BY m.fqn LIMIT ${limit}`
          return { content: await runCypher(merged, cypher) }
        },
      })
    })
  },
})
