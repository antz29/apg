import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Mark a plan task done: `apg plan done <project> <task-fqn>`. The implementer's assertion ONLY — no promotion, no code-graph verification (the plan's planned nodes stay declared until the verify gate's coherence check). Use apg_plan_undone to reverse.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    project: tool.schema.string().describe("Plan project (required)."),
    task: tool.schema
      .string()
      .describe("Task FQN or short id, e.g. <project>/plan.phase-01.task-1 or plan.phase-01.task-1 (required)."),
  },
  async execute(args, context) {
    const { project, task } = args
    if (!project || !task) return "Error: project and task are required"
    const fqn = task.includes("/") ? task : `${project}/${task.replace(/^plan\./, "plan.")}`
    return runCli(context, ["plan", "done", project, fqn], args.directory)
  },
})