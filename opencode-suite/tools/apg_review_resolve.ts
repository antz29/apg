import { Plugin } from "@opencode/plugin"
import { resolveProjectWorktree, runInProject, type ToolContext } from "../lib/apg.ts"

type ApgReviewResolveInput = { project: string; feedback: string }

export default Plugin.define({
  id: "apg_review_resolve",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_review_resolve",
        options: { codemode: false },
        description:
          "A reviewer resolves an actioned review item (`apg review resolve <feedback-fqn>`): terminal — status becomes resolved and the item no longer blocks the phase-complete milestone or the verify/merge gate. Only the reviewer side does this; writers cannot resolve their own feedback.",
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
          const input = raw as ApgReviewResolveInput
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
          return { content: await runInProject(merged, ["review", "resolve", feedback]) }
        },
      })
    })
  },
})
