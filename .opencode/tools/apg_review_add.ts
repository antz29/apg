import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Attach review feedback to an artifact node (`apg review add <target-fqn> --body ... [--checks <invariant-fqn>]*`): a spec/requirement/phase/future/plan/task node or a code node. Creates an open Feedback. The target must exist in the graph; a code target requires --project to route the feedback into the plan JSONL. --checks cites the invariants the comment enforces (a Checks Feedback→Invariant edge); most feedback has none.",
  args: {
    target: tool.schema
      .string()
      .describe("FQN of the node being reviewed, e.g. <project>/spec.R1 or a code FQN (required)."),
    body: tool.schema.string().describe("The review comment / issue (required)."),
    project: tool.schema
      .string()
      .optional()
      .describe("Required when target is a code FQN (feedback routes to the plan JSONL)."),
    checks: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("Invariant FQNs this comment enforces (Checks Feedback→Invariant edges)."),
  },
  async execute(args, context) {
    const { target, body } = args
    if (!target || !body) return "Error: target and body are required"
    const cli = ["review", "add", target, "--body", body]
    if (args.project) cli.push("--project", args.project)
    for (const c of args.checks ?? []) cli.push("--checks", c)
    return runCli(context, cli)
  },
})