import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

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
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            project: { type: "string", description: "Plan project (required)." },
            phaseNumber: { type: "string", description: "The phase number to complete (required)." },
          },
          required: ["project", "phaseNumber"],
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
          const { project, phaseNumber } = input
          if (!project || !phaseNumber) return { content: "Error: project and phaseNumber are required" }
          return { content: await runCli(merged, ["plan", "complete", project, phaseNumber], input.directory) }
        },
      })
    })
  },
})
