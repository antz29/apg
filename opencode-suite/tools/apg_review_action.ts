import { Plugin } from "@opencode/plugin"
import { resolveProjectWorktree, runInProject, type ToolContext } from "../lib/apg.ts"

type ApgReviewActionInput = { project: string; feedback: string; disposition: string }

export default Plugin.define({
  id: "apg_review_action",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_review_action",
        options: { codemode: false },
        description:
          "The coordinator actions review feedback (`apg review action <feedback-fqn> --fix|--wont-fix`): marks it actioned with disposition fixed or wont-fix. The coordinator runs this on the owning writer's ACTIONED/WONT-FIX claim, after a shallow claim-vs-change consistency check. Reviewers attach/resolve/reject and never action; writers return a claim only.",
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
            disposition: {
              type: "string",
              description: 'How it was handled: "fix" or "wont-fix" (required).',
            },
          },
          required: ["project", "feedback", "disposition"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgReviewActionInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const { feedback, disposition } = input
          if (!feedback || !disposition) return { content: "Error: feedback and disposition are required" }
          if (disposition !== "fix" && disposition !== "wont-fix") {
            return { content: 'Error: disposition must be "fix" or "wont-fix"' }
          }
          return {
            content: await runInProject(merged, ["review", "action", feedback, `--${disposition}`]),
          }
        },
      })
    })
  },
})
