import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Reclassify a plan task's owning role (kind) and optional verification depth: `apg plan retag <project> <task-fqn> --kind <source|test|gate|docs> [--tier <unit|int|e2e>]`. Validates the two-axis classification (a `test` task must get a `tier`; a non-test task cannot) and write-throughs the plan JSONL. Use when a task's role was mis-set at authoring time — e.g. the retired `human` kind: tasks are implementer-workable, the human's decision point is plan end, never a phase task.",
  args: {
    project: tool.schema.string().describe("Plan project (required)."),
    task: tool.schema
      .string()
      .describe("Task FQN or short id, e.g. plan.phase-03.task-5 or future/<project>/plan.phase-03.task-5 (required)."),
    kind: tool.schema
      .string()
      .describe("New owning role: source, test, gate, or docs (required)."),
    tier: tool.schema
      .string()
      .optional()
      .describe("Verification depth: unit, int, or e2e. Required when kind=test."),
  },
  async execute(args, context) {
    const { project, task, kind, tier } = args
    if (!project || !task || !kind) return "Error: project, task, and kind are required"
    const fqn = task.startsWith("future/") ? task : `future/${project}/${task}`
    const cli = ["plan", "retag", project, fqn, "--kind", kind]
    if (tier) cli.push("--tier", tier)
    return runCli(context, cli)
  },
})