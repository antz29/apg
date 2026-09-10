import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Attach a task note: `apg plan note <project> <task-fqn> --body …`. An implementer records a concern or deviation that arose during execution. The note serializes into the transient plan JSONL (branch-local, survives until the verify gate) and is surfaced to the human at the merge handoff.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    project: tool.schema.string().describe("Plan project (required)."),
    task: tool.schema
      .string()
      .describe("Task FQN or short id, e.g. <project>/plan.phase-01.task-1 or plan.phase-01.task-1 (required)."),
    body: tool.schema.string().describe("The note body (required)."),
  },
  async execute(args, context) {
    const { project, task, body } = args
    if (!project || !task || !body) return "Error: project, task, and body are required"
    const fqn = task.includes("/") ? task : `${project}/${task.replace(/^plan\./, "plan.")}`
    return runCli(context, ["plan", "note", project, fqn, "--body", body], args.directory)
  },
})