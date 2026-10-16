import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_review_reject",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_review_reject",
        description:
          "A reviewer rejects an actioned review item (`apg review reject <feedback-fqn>`): reopens it (status back to open, disposition rejected) so the writer must rework. Only the reviewer side does this.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            feedback: {
              type: "string",
              description: "Feedback FQN, e.g. <project>/feedback-1 (required).",
            },
          },
          required: ["feedback"],
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
          const { feedback } = input
          if (!feedback) return { content: "Error: feedback is required" }
          return { content: await runCli(merged, ["review", "reject", feedback], input.directory) }
        },
      })
    })
  },
})
