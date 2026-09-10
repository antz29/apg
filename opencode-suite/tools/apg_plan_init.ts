import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Create a plan: `apg plan init <project> [--title T] [--strategy S]`. The plan is the tier-4 bridge from the spec's proposed reality (the durable requirements/domain/solution node files under apg/layers/) to present code; the plan-writer authors the planned Implementation nodes. It serializes to the transient apg/.trans/plans/<project>.jsonl (never committed). A plan with no requirement nodes yet is allowed with a warning — phases added with --satisfies validate against apg/layers/requirements/.",
  args: {
    project: tool.schema.string().describe("Plan project — the branch/worktree name (required)."),
    title: tool.schema.string().optional().describe("Plan title (defaults to the project name)."),
    strategy: tool.schema.string().optional().describe("Strategy text (variants, tier routing, gates, execution method)."),
  },
  async execute(args, context) {
    const project = args.project
    if (!project) return "Error: project is required"
    const cli = ["plan", "init", project]
    if (args.title) cli.push("--title", args.title)
    if (args.strategy) cli.push("--strategy", args.strategy)
    return runCli(context, cli)
  },
})