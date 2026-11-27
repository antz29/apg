import { Plugin } from "@opencode/plugin"
import { resolveProjectWorktree, runInProject, type ToolContext } from "../lib/apg.ts"

type ApgReviewRejectInput = { project: string; feedback: string }

export default Plugin.define({
  id: "apg_review_reject",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_review_reject",
        options: { codemode: false },
        description:
          "A reviewer rejects an actioned review item (`apg review reject <feedback-fqn>`): reopens it (status back to open, disposition rejected) so the writer must rework. Only the reviewer side does this.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            feedback: {
              type: "string",
              description: "Feedback FQN, e.g. <project>/feedback-1 (required).",
            },
          },
          required: ["project", "feedback"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgReviewRejectInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const { feedback } = input
          if (!feedback) return { content: "Error: feedback is required" }
          return { content: await runInProject(merged, ["review", "reject", feedback]) }
        },
      })
    })
  },
})
