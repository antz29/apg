import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "A reviewer resolves an actioned review item (`apg review resolve <feedback-fqn>`): terminal — status becomes resolved and the item no longer blocks archive/complete. Only the reviewer side does this; writers cannot resolve their own feedback.",
  args: {
    feedback: tool.schema
      .string()
      .describe("Feedback FQN, e.g. future/<project>/feedback-1 (required)."),
  },
  async execute(args, context) {
    const { feedback } = args
    if (!feedback) return "Error: feedback is required"
    return runCli(context, ["review", "resolve", feedback])
  },
})