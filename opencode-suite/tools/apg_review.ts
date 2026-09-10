import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "List review feedback: every Feedback with its status/disposition and the node it reviews, or filtered to one target (`apg review list [<target-fqn>]`). Feedback is transient (branch-local apg/.trans mirrors); read-only.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    target: tool.schema
      .string()
      .optional()
      .describe("Only list feedback reviewing this node FQN (a layer node, plan/phase/task, or code)."),
  },
  async execute(args, context) {
    const cli = ["review", "list"]
    if (args.target) cli.push(args.target)
    return runCli(context, cli, args.directory)
  },
})
