import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Flip a done plan task back to pending: `apg plan undone <project> <task-fqn>`. A checklist correction only — it does not recreate retired Futures (the code stays in the present).",
  args: {
    project: tool.schema.string().describe("Plan project (required)."),
    task: tool.schema
      .string()
      .describe("Task FQN or short id, e.g. future/<project>/plan.phase-01.task-1 or plan.phase-01.task-1 (required)."),
  },
  async execute(args, context) {
    const { project, task } = args
    if (!project || !task) return "Error: project and task are required"
    const fqn = task.startsWith("future/") ? task : `future/${project}/${task}`
    return runCli(context, ["plan", "undone", project, fqn])
  },
})