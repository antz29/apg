import { Plugin } from "@opencode/plugin"
import { runCypher, lit, codeTypeCondition, noteIfEmpty, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_callers",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_callers",
        description:
          "Find every function that calls the given function (incoming Calls edges). Include the caller's location so you can jump to each call site.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            fqn: {
              type: "string",
              description: "Target function FQN (language-rooted), e.g. java.org.jgrapht.Graph.addVertex (required)",
            },
            codeType: {
              type: "string",
              description: 'Code type filter for callers: "src", "test", "generated", "external". Defaults to "all".',
            },
            limit: { type: "string", description: "Max results (default 500, max 1000)" },
          },
          required: ["fqn"],
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
          const fqn = input.fqn
          if (!fqn) return { content: "Error: fqn is required" }
          const limit = input.limit ? Math.max(1, Math.min(1000, Number(input.limit))) : 500

          let cypher = `MATCH (c:Function)-[:Calls]->(t:Function {fqn: ${lit(fqn)}})`
          const ctCond = codeTypeCondition("c", input.codeType)
          if (ctCond) cypher += ` WHERE ${ctCond}`
          cypher += ` RETURN c.fqn, c.path, c.start_line, c.end_line, c.code_type ORDER BY c.fqn LIMIT ${limit}`
          // Column 1 is the caller's source-file `path` (stored repo-relative).
          const out = await runCypher(merged, cypher, input.directory, { rebaseColumns: [1] })
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
