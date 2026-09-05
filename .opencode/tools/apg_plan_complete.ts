import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Complete a plan phase: `apg plan complete <project> <phase-n>`. Requires every phase task done AND all phase/task feedback resolved; then adds Implements edges for each Satisfies(PlanPhase→Requirement). Completing the final phase retires the plan (its JSONL is dropped — plans are transient).",
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