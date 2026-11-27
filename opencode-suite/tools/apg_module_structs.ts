import { Plugin } from "@opencode/plugin"
import { runCypher, lit, codeTypeCondition, resolveProjectWorktree, type ToolContext } from "../lib/apg.ts"

type ApgModuleStructsInput = { project: string; fqn: string; codeType?: string; limit?: string }

export default Plugin.define({
  id: "apg_module_structs",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_module_structs",
        options: { codemode: false },
        description:
          "List every struct/class/interface/enum declared anywhere under a module (two hops through its files). Use for 'all types in a package' style questions.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            fqn: { type: "string", description: "Module FQN (language-rooted), e.g. java.org.jgrapht.alg (required)" },
            codeType: {
              type: "string",
              description: 'Code type filter: "src", "test", "generated", "external". Defaults to "all".',
            },
            limit: { type: "string", description: "Max results (default 500, max 1000)" },
          },
          required: ["project", "fqn"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgModuleStructsInput
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

          let cypher =
            `MATCH (m:Module {fqn: ${lit(fqn)}})-[:Contains]->(:File)-[:Contains]->(s:Struct)`
          const ctCond = codeTypeCondition("s", input.codeType)
          if (ctCond) cypher += ` WHERE ${ctCond}`
          cypher += ` RETURN s.fqn, s.path, s.start_line, s.end_line, s.code_type ORDER BY s.fqn LIMIT ${limit}`
          // Column 1 is the struct's source-file `path` (stored repo-relative).
          return { content: await runCypher(merged, cypher, undefined, { rebaseColumns: [1] }) }
        },
      })
    })
  },
})
