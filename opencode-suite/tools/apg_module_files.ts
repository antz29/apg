import { Plugin } from "@opencode/plugin"
import { runCypher, lit, codeTypeCondition, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_module_files",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_module_files",
        options: { codemode: false },
        description:
          "List the source files in a module (package/directory), with their line counts. Each file's fqn is its absolute path; use apg_file_units to see what a file contains.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            fqn: { type: "string", description: "Module FQN (language-rooted), e.g. java.org.jgrapht.alg or go.github.com/org/repo (required)" },
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

          let cypher = `MATCH (m:Module {fqn: ${lit(fqn)}})-[:Contains]->(f:File)`
          const ctCond = codeTypeCondition("f", input.codeType)
          if (ctCond) cypher += ` WHERE ${ctCond}`
          cypher += ` RETURN f.fqn, f.start_line, f.end_line, f.code_type ORDER BY f.fqn LIMIT ${limit}`
          // Column 0 is the File fqn — the stored repo-relative identity. Resolve it
          // to an absolute path under the caller's project directory.
          return { content: await runCypher(merged, cypher, input.directory, { rebaseColumns: [0] }) }
        },
      })
    })
  },
})
