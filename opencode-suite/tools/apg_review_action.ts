import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_review_action",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_review_action",
        description:
          "The coordinator actions review feedback (`apg review action <feedback-fqn> --fix|--wont-fix`): marks it actioned with disposition fixed or wont-fix. The coordinator runs this on the owning writer's ACTIONED/WONT-FIX claim, after a shallow claim-vs-change consistency check. Reviewers attach/resolve/reject and never action; writers return a claim only.",
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
            disposition: {
              type: "string",
              description: 'How it was handled: "fix" or "wont-fix" (required).',
            },
          },
          required: ["feedback", "disposition"],
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
          const { feedback, disposition } = input
          if (!feedback || !disposition) return { content: "Error: feedback and disposition are required" }
          if (disposition !== "fix" && disposition !== "wont-fix") {
            return { content: 'Error: disposition must be "fix" or "wont-fix"' }
          }
          return {
            content: await runCli(merged, ["review", "action", feedback, `--${disposition}`], input.directory),
          }
        },
      })
    })
  },
})
