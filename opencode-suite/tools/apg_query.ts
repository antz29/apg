import { Plugin } from "@opencode/plugin"
import { runCypher, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_query",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_query",
        description:
          "Execute a read-only Cypher query on the project's LadybugDB program graph (apg/.trans/db.lbug). CSV output, header row included. Graph schema: Module ->(Contains)-> File ->(Contains)-> Struct/Function; Struct ->(Contains)-> Struct/Function for nested types and methods; Calls/Uses/Unresolved* edges link functions and types. Struct/Function/File nodes carry start_line/end_line (1-based) plus path — use them to join against diffs/hunks. MATCH/RETURN only. No modifications.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            query: {
              type: "string",
              description:
                "Cypher query, e.g. MATCH (f:File {fqn:'/abs/Graph.java'})-[:Contains]->(n) RETURN labels(n), n.fqn, n.start_line, n.end_line",
            },
          },
          required: ["query"],
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
          return { content: await runCypher(merged, input.query, input.directory) }
        },
      })
    })
  },
})
