import { Plugin } from "@opencode/plugin"
import { runCypher, lit, codeTypeCondition, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_module_structs",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_module_structs",
        description:
          "List every struct/class/interface/enum declared anywhere under a module (two hops through its files). Use for 'all types in a package' style questions.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            fqn: { type: "string", description: "Module FQN (language-rooted), e.g. java.org.jgrapht.alg (required)" },
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

          let cypher =
            `MATCH (m:Module {fqn: ${lit(fqn)}})-[:Contains]->(:File)-[:Contains]->(s:Struct)`
          const ctCond = codeTypeCondition("s", input.codeType)
          if (ctCond) cypher += ` WHERE ${ctCond}`
          cypher += ` RETURN s.fqn, s.path, s.start_line, s.end_line, s.code_type ORDER BY s.fqn LIMIT ${limit}`
          // Column 1 is the struct's source-file `path` (stored repo-relative).
          return { content: await runCypher(merged, cypher, input.directory, { rebaseColumns: [1] }) }
        },
      })
    })
  },
})
