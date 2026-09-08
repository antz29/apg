import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Add a phase, task, or planned Implementation node to a plan (`apg plan add <project> phase|task|planned …`). Phase: number, title, deliverable, prereq (phase numbers), satisfies (requirement ids the phase delivers). Task: phase number, task number, title, kind (source/test/gate/docs — owning role), tier (unit/int/e2e — required for test tasks only), builds (the FQN of a planned node this task creates), anchor (code FQNs touched). Planned node: kind (module/file/struct/function), fqn (the real code FQN where the code will land), optional name + parent — the plan-writer's tier-4 additions (GraphModel-SPEC).",
  args: {
    project: tool.schema.string().describe("Plan project (required)."),
    kind: tool.schema.string().describe('"phase", "task", or "planned" (required).'),
    number: tool.schema.string().optional().describe("For phase: the phase number."),
    phaseNumber: tool.schema.string().optional().describe("For task: the phase number the task belongs to."),
    taskNumber: tool.schema.string().optional().describe("For task: the task number within the phase."),
    title: tool.schema.string().optional().describe("Phase or task title (required)."),
    deliverable: tool.schema.string().optional().describe("For phase: one-line deliverable."),
    prereq: tool.schema.array(tool.schema.string()).optional().describe("For phase: phase numbers this phase is gated on (prereqs)."),
    satisfies: tool.schema.array(tool.schema.string()).optional().describe("For phase: requirement ids this phase delivers (Satisfies)."),
    taskKind: tool.schema
      .string()
      .optional()
      .describe("For task: the owning role — source (default), test, gate, or docs."),
    tier: tool.schema
      .string()
      .optional()
      .describe("For task: verification depth — unit, int, or e2e. Only valid with taskKind=test (required then)."),
    builds: tool.schema.string().optional().describe("For task: the FQN of the planned Implementation node this task builds."),
    anchor: tool.schema.array(tool.schema.string()).optional().describe("For task: code FQNs this task touches."),
    plannedKind: tool.schema
      .string()
      .optional()
      .describe('For planned node: module, file, struct, or function (required).'),
    fqn: tool.schema.string().optional().describe("For planned node: the real code FQN where the code will land (required)."),
    name: tool.schema.string().optional().describe("For planned node: the simple name."),
    parent: tool.schema.string().optional().describe("For planned node: the containing node FQN."),
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
      if (args.taskKind) cli.push("--kind", args.taskKind)
      if (args.tier) cli.push("--tier", args.tier)
      if (args.builds) cli.push("--builds", args.builds)
      for (const a of args.anchor ?? []) cli.push("--anchor", a)
    } else if (kind === "planned") {
      if (!args.plannedKind || !args.fqn) return "Error: planned node requires plannedKind and fqn"
      cli.push(args.plannedKind, args.fqn)
      if (args.name) cli.push("--name", args.name)
      if (args.parent) cli.push("--parent", args.parent)
    } else {
      return "Error: kind must be \"phase\", \"task\", or \"planned\""
    }
    return runCli(context, cli)
  },
})