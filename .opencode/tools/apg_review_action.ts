import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "A writer actions review feedback (`apg review action <feedback-fqn> --fix|--wont-fix`): marks it actioned with disposition fixed or wont-fix. Only the writer side does this — reviewers attach/resolve/reject, never action.",
  args: {
    feedback: tool.schema
      .string()
      .describe("Feedback FQN, e.g. future/<project>/feedback-1 (required)."),
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
    return runCli(context, ["review", "action", feedback, `--${disposition}`])
  },
})