import { Plugin } from "@opencode/plugin"
import { resolveProjectWorktree, runInProject, type ToolContext } from "../lib/apg.ts"

type ApgPlanCompleteInput = { project: string; phaseNumber: string }

export default Plugin.define({
  id: "apg_plan_complete",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan_complete",
        options: { codemode: false },
        description:
          "Complete a plan phase: `apg plan complete <project> <phase-n>`. A MILESTONE only — requires every phase task done AND all phase/task feedback resolved, then marks the phase complete. Nothing is materialized and the plan is not retired: the plan survives until the verify gate (`apg plan verify`) and the merge act (`apg project merge` from the main checkout).",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            phaseNumber: { type: "string", description: "The phase number to complete (required)." },
          },
          required: ["project", "phaseNumber"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgPlanCompleteInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const { project, phaseNumber } = input
          if (!project || !phaseNumber) return { content: "Error: project and phaseNumber are required" }
          return { content: await runInProject(merged, ["plan", "complete", project, phaseNumber]) }
        },
      })
    })
  },
})
