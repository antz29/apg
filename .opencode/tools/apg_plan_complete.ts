import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Complete a plan phase: `apg plan complete <project> <phase-n>`. A MILESTONE only — requires every phase task done AND all phase/task feedback resolved, then marks the phase complete. NO Implements materialization and NO plan retirement: the plan survives until the apply act (apg_plan_apply), which materializes delivery records in one act (PlanCompletion-SPEC.md).",
  args: {
    project: tool.schema.string().describe("Plan project (required)."),
    phaseNumber: tool.schema.string().describe("The phase number to complete (required)."),
  },
  async execute(args, context) {
    const { project, phaseNumber } = args
    if (!project || !phaseNumber) return "Error: project and phaseNumber are required"
    return runCli(context, ["plan", "complete", project, phaseNumber])
  },
})