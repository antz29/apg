import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "The coordinator actions review feedback (`apg review action <feedback-fqn> --fix|--wont-fix`): marks it actioned with disposition fixed or wont-fix. The coordinator runs this on the owning writer's ACTIONED/WONT-FIX claim, after a shallow claim-vs-change consistency check. Reviewers attach/resolve/reject and never action; writers return a claim only.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    feedback: tool.schema
      .string()
      .describe("Feedback FQN, e.g. <project>/feedback-1 (required)."),
    disposition: tool.schema
      .string()
      .describe('How it was handled: "fix" or "wont-fix" (required).'),
  },
  async execute(args, context) {
    const { feedback, disposition } = args
    if (!feedback || !disposition) return "Error: feedback and disposition are required"
    if (disposition !== "fix" && disposition !== "wont-fix") {
      return "Error: disposition must be \"fix\" or \"wont-fix\""
    }
    return runCli(context, ["review", "action", feedback, `--${disposition}`], args.directory)
  },
})