import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Attach review feedback to an artifact node (`apg review add <target-fqn> --body ... [--project <p>]`): a durable layer node (`<layer>.<type>.<name>`, e.g. requirements.requirement.place-order), a plan/phase/task FQN (`<project>/plan…`), or a code FQN. Creates an open Feedback. Feedback is transient — it routes into the branch's apg/.trans mirror (plan targets into the plan store, durable/code targets into the tier mirror) and dies with the branch. The target must exist in the graph; durable-layer and code targets need --project to route the feedback.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    target: tool.schema
      .string()
      .describe("FQN of the node being reviewed — a layer node (`requirements.requirement.x`), a plan/phase/task FQN, or a code FQN (required)."),
    body: tool.schema.string().describe("The review comment / issue (required)."),
    project: tool.schema
      .string()
      .optional()
      .describe("Required when the target is a code FQN or a durable layer FQN (routes the feedback into the branch's .trans mirror)."),
  },
  async execute(args, context) {
    const { target, body } = args
    if (!target || !body) return "Error: target and body are required"
    const cli = ["review", "add", target, "--body", body]
    if (args.project) cli.push("--project", args.project)
    return runCli(context, cli, args.directory)
  },
})
