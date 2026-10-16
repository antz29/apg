import { Plugin } from "@opencode/plugin"
import { runCypher, lit, codeTypeCondition, noteIfEmpty, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_methods",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_methods",
        options: { codemode: false },
        description:
          "List the methods and functions of a struct/class/interface/enum (its Struct -> Function containment), with file locations and line ranges.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            fqn: {
              type: "string",
              description: "Struct FQN (language-rooted), e.g. java.org.jgrapht.Graph (required)",
            },
            codeType: {
              type: "string",
              description: 'Code type filter: "src", "test", "generated", "external". Defaults to "all".',
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

          let cypher = `MATCH (s:Struct {fqn: ${lit(fqn)}})-[:Contains]->(f:Function)`
          const ctCond = codeTypeCondition("f", input.codeType)
          if (ctCond) cypher += ` WHERE ${ctCond}`
          cypher += ` RETURN f.fqn, f.path, f.start_line, f.end_line, f.code_type ORDER BY f.fqn LIMIT ${limit}`
          // Column 1 is the function's source-file `path` (stored repo-relative).
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
