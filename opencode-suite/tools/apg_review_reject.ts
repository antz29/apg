import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "A reviewer rejects an actioned review item (`apg review reject <feedback-fqn>`): reopens it (status back to open, disposition rejected) so the writer must rework. Only the reviewer side does this.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    feedback: tool.schema
      .string()
      .describe("Feedback FQN, e.g. <project>/feedback-1 (required)."),
  },
  async execute(args, context) {
    const { feedback } = args
    if (!feedback) return "Error: feedback is required"
    return runCli(context, ["review", "reject", feedback], args.directory)
  },
})