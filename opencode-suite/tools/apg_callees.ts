import { Plugin } from "@opencode/plugin"
import { runCypher, lit, codeTypeCondition, noteIfEmpty, resolveProjectWorktree, type ToolContext } from "../lib/apg.ts"

type ApgCalleesInput = { project: string; fqn: string; codeType?: string; limit?: string }

export default Plugin.define({
  id: "apg_callees",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_callees",
        options: { codemode: false },
        description:
          "Find every function the given function calls (outgoing Calls edges). Use to trace a function's dependencies or expand its call tree.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            fqn: { type: "string", description: "Source function FQN (required)" },
            codeType: {
              type: "string",
              description: 'Code type filter for callees: "src", "test", "generated", "external". Defaults to "all".',
            },
            limit: { type: "string", description: "Max results (default 500, max 1000)" },
          },
          required: ["project", "fqn"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgCalleesInput
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
          if (!fqn) return { content: "Error: fqn is required" }
          const limit = input.limit ? Math.max(1, Math.min(1000, Number(input.limit))) : 500

          let cypher = `MATCH (f:Function {fqn: ${lit(fqn)}})-[:Calls]->(c:Function)`
          const ctCond = codeTypeCondition("c", input.codeType)
          if (ctCond) cypher += ` WHERE ${ctCond}`
          cypher += ` RETURN c.fqn, c.path, c.start_line, c.end_line, c.code_type ORDER BY c.fqn LIMIT ${limit}`
          // Column 1 is the callee's source-file `path` (stored repo-relative).
          const out = await runCypher(merged, cypher, undefined, { rebaseColumns: [1] })
          return {
            content: noteIfEmpty(
              out,
              "no results (FQN is exact — overloads carry parameter suffixes; use apg_find_symbol to locate one)",
            ),
          }
        },
      })
    })
  },
})
