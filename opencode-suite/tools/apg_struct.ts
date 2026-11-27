import { Plugin } from "@opencode/plugin"
import { runCypher, lit, resolveProjectWorktree, type ToolContext } from "../lib/apg.ts"

type ApgStructInput = { project: string; fqn: string }

export default Plugin.define({
  id: "apg_struct",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_struct",
        options: { codemode: false },
        description:
          "Show a struct/class/interface/enum: its file location and line range, plus any nested structs it directly contains.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            fqn: {
              type: "string",
              description:
                "Struct FQN (language-rooted), e.g. java.org.jgrapht.graph.DefaultGraphType (required)",
            },
          },
          required: ["project", "fqn"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgStructInput
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

          const self =
            `MATCH (s:Struct {fqn: ${lit(fqn)}}) ` +
            `RETURN s.fqn, s.path, s.start_line, s.end_line, s.code_type`
          const nested =
            `MATCH (s:Struct {fqn: ${lit(fqn)}})-[:Contains]->(n:Struct) ` +
            `RETURN n.fqn, n.start_line, n.end_line ORDER BY n.start_line`

          const [selfOut, nestedOut] = await Promise.all([
            // Column 1 is `s.path`, the stored repo-relative source-file identity.
            runCypher(merged, self, { rebaseColumns: [1] }),
            runCypher(merged, nested),
          ])
          const nestedBody = nestedOut.split("\n").slice(1).join("\n")
          return { content: nestedBody ? `${selfOut}\n-- nested structs --\n${nestedBody}` : selfOut }
        },
      })
    })
  },
})
