import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Create a plan: `apg plan init <project> [--title T] [--strategy S]`. The plan is the bridge from the spec's future nodes to present code; it serializes to the transient apg/.trans/plans/<project>.jsonl. Requires a spec to exist first.",
  args: {
    project: tool.schema.string().describe("Plan project (must match a spec project) (required)."),
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