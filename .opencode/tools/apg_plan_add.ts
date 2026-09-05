import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Add a phase or task to a plan (`apg plan add <project> phase|task …`). Phase: number, title, deliverable, prereq (phase numbers), satisfies (requirement ids the phase delivers). Task: phase number, task number, title, tier (source/unit/int/e2e/gate/human), builds (future name the task creates), anchor (code FQNs touched).",
  args: {
    project: tool.schema.string().describe("Plan project (required)."),
    kind: tool.schema.string().describe('"phase" or "task" (required).'),
    number: tool.schema.string().optional().describe("For phase: the phase number."),
    phaseNumber: tool.schema.string().optional().describe("For task: the phase number the task belongs to."),
    taskNumber: tool.schema.string().optional().describe("For task: the task number within the phase."),
    title: tool.schema.string().optional().describe("Phase or task title (required)."),
    deliverable: tool.schema.string().optional().describe("For phase: one-line deliverable."),
    prereq: tool.schema.array(tool.schema.string()).optional().describe("For phase: phase numbers this phase is gated on (prereqs)."),
    satisfies: tool.schema.array(tool.schema.string()).optional().describe("For phase: requirement ids this phase delivers (Satisfies)."),
    tier: tool.schema
      .string()
      .optional()
      .describe("For task: source, unit, int, e2e, gate, or human (routes the owning writer)."),
    builds: tool.schema.string().optional().describe("For task: the future name this task builds."),
    anchor: tool.schema.array(tool.schema.string()).optional().describe("For task: code FQNs this task touches."),
  },
  async execute(args, context) {
    const { project, kind } = args
    if (!project || !kind) return "Error: project and kind are required"
    const cli = ["plan", "add", project, kind]

    if (kind === "phase") {
      if (!args.number) return "Error: phase requires number"
      if (!args.title) return "Error: phase requires title"
      cli.push(args.number, "--title", args.title)
      if (args.deliverable) cli.push("--deliverable", args.deliverable)
      for (const p of args.prereq ?? []) cli.push("--prereq", p)
      for (const s of args.satisfies ?? []) cli.push("--satisfies", s)
    } else if (kind === "task") {
      if (!args.phaseNumber || !args.taskNumber) return "Error: task requires phaseNumber and taskNumber"
      if (!args.title) return "Error: task requires title"
      cli.push(args.phaseNumber, args.taskNumber, "--title", args.title)
      if (args.tier) cli.push("--tier", args.tier)
      if (args.builds) cli.push("--builds", args.builds)
      for (const a of args.anchor ?? []) cli.push("--anchor", a)
    } else {
      return "Error: kind must be \"phase\" or \"task\""
    }
    return runCli(context, cli)
  },
})