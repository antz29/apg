import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Complete a plan phase: `apg plan complete <project> <phase-n>`. A MILESTONE only — requires every phase task done AND all phase/task feedback resolved, then marks the phase complete. Nothing is materialized and the plan is not retired: the plan survives until the verify gate (`apg plan verify`) and the merge act (`apg project merge` from the main checkout).",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    project: tool.schema.string().describe("Plan project (required)."),
    phaseNumber: tool.schema.string().describe("The phase number to complete (required)."),
  },
  async execute(args, context) {
    const { project, phaseNumber } = args
    if (!project || !phaseNumber) return "Error: project and phaseNumber are required"
    return runCli(context, ["plan", "complete", project, phaseNumber], args.directory)
  },
})