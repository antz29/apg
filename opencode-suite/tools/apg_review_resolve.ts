import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_review_resolve",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_review_resolve",
        description:
          "A reviewer resolves an actioned review item (`apg review resolve <feedback-fqn>`): terminal — status becomes resolved and the item no longer blocks the phase-complete milestone or the verify/merge gate. Only the reviewer side does this; writers cannot resolve their own feedback.",
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
          return { content: await runCli(merged, ["review", "resolve", feedback], input.directory) }
        },
      })
    })
  },
})
