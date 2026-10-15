import { Plugin } from "@opencode/plugin"
import { runCypher, lit, codeTypeCondition, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_uses",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_uses",
        description:
          "Follow Uses edges (type references) for a symbol. direction=out lists the types a function/struct uses; direction=in lists who uses a struct. Note: calls to a struct's methods appear as Calls, not Uses.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            fqn: { type: "string", description: "Symbol FQN (required)" },
            direction: {
              type: "string",
              description: '"out" (default): types X uses. "in": units that use X.',
            },
            codeType: {
              type: "string",
              description:
                'Code type filter on the OTHER end of the edge (the used type for "out", the user for "in"). Defaults to "all".',
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
          const direction = input.direction || "out"
          const limit = input.limit ? Math.max(1, Math.min(1000, Number(input.limit))) : 500

          let cypher: string
          // The `n.path` / `s.path` column is the stored repo-relative source-file
          // identity; its CSV index differs per branch.
          let rebaseColumns: number[]
          if (direction === "in") {
            cypher = `MATCH (n)-[:Uses]->(s:Struct) WHERE s.fqn = ${lit(fqn)}`
            const ctCond = codeTypeCondition("n", input.codeType)
            if (ctCond) cypher += ` AND ${ctCond}`
            cypher += ` RETURN labels(n) as kind, n.fqn as user, n.path, n.start_line, n.end_line ORDER BY n.fqn LIMIT ${limit}`
            rebaseColumns = [2]
          } else {
            cypher = `MATCH (n)-[:Uses]->(s:Struct) WHERE n.fqn = ${lit(fqn)}`
            const ctCond = codeTypeCondition("s", input.codeType)
            if (ctCond) cypher += ` AND ${ctCond}`
            cypher += ` RETURN s.fqn as used_type, s.path, s.start_line, s.end_line, s.code_type ORDER BY s.fqn LIMIT ${limit}`
            rebaseColumns = [1]
          }
          return { content: await runCypher(merged, cypher, input.directory, { rebaseColumns }) }
        },
      })
    })
  },
})
