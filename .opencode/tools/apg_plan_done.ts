import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Mark a plan task done: `apg plan done <project> <task-fqn>`. For each Builds(Task→Future) whose target exists in the code graph this promotes the future (re-anchors requirements, adds Implements edges, retires the Future) and flips the task to done. Errors if a Builds future's target does not resolve — a task cannot be done until its code actually exists.",
  args: {
    project: tool.schema.string().describe("Plan project (required)."),
    task: tool.schema
      .string()
      .describe("Task FQN or short id, e.g. future/<project>/plan.phase-01.task-1 or plan.phase-01.task-1 (required)."),
  },
  async execute(args, context) {
    const { project, task } = args
    if (!project || !task) return "Error: project and task are required"
    const fqn = task.startsWith("future/") ? task : `future/${project}/${task.replace(/^plan\./, "plan.")}`
    return runCli(context, ["plan", "done", project, fqn])
  },
})