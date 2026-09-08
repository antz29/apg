import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Add Satisfies and/or prereq (Gates) edges to a plan phase: `apg plan link <project> <phase-n> [--satisfies <req-id>]* [--prereq <phase-n>]*`. Satisfies marks the requirements the phase's deliverable fulfils; prereq adds a Gates edge.",
  args: {
    project: tool.schema.string().describe("Plan project (required)."),
    phaseNumber: tool.schema.string().describe("The phase number to link (required)."),
    satisfies: tool.schema.array(tool.schema.string()).optional().describe("Requirement ids this phase delivers."),
    prereq: tool.schema.array(tool.schema.string()).optional().describe("Phase numbers this phase is gated on."),
  },
  async execute(args, context) {
    const { project, phaseNumber, satisfies, prereq } = args
    if (!project || !phaseNumber) return "Error: project and phaseNumber are required"
    if (!satisfies?.length && !prereq?.length) return "Error: provide at least one satisfies or prereq"
    const cli = ["plan", "link", project, phaseNumber]
    for (const s of satisfies ?? []) cli.push("--satisfies", s)
    for (const p of prereq ?? []) cli.push("--prereq", p)
    return runCli(context, cli)
  },
})